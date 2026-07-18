//! Claude Code hook adapter.
//!
//! Classifies the blocking events (`PermissionRequest`, `PreToolUse:
//! ExitPlanMode`, `PreToolUse: AskUserQuestion`) and the lifecycle events
//! (`SessionStart` registers idle, `UserPromptSubmit` moves to running with
//! the prompt as task, `Stop` completes the turn — success, or failed on an
//! error signal, or back to running when `background_tasks` or `session_crons`
//! still has work pending, `SessionEnd` exits, `Notification` silent);
//! renders the Claude-shaped `hookSpecificOutput` / `updatedInput` decision
//! payload and the silent neutral fallback. Context budget is read from the
//! transcript tail.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers. The
//! `PermissionRequest` blocking hook is marked `_rimz_sync = true`; an existing
//! async marker on it is a hard install error (see [`CLAUDE_HOOKS`] and
//! `docs/internals/agents/claude.md`). The `PreToolUse` blocking sub-events ride the
//! broad `PreToolUse` hook and self-classify from `tool_name`.

pub(crate) mod account;
mod ask;
mod install;
mod local_sessions;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub mod remote_control;
pub(crate) mod spend;
mod statusline;
mod subagent_statusline;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
#[cfg(test)]
use serde_json::Map;
use serde_json::Value;

use self::install::MANAGED_SOURCE;
#[cfg(test)]
use self::install::{classify_status_line_change, upsert_rimz_status_line};
#[cfg(test)]
use self::install::{read_existing_json, wrapped_status_line_command_from};
use self::payloads::{
    ClaudeCommon, ClaudePermissionRequest, ClaudePostCompact, ClaudePostToolUse, ClaudePreToolUse,
    ClaudeSessionStart, ClaudeStop, ClaudeStopFailure, ClaudeSubagentStart, ClaudeSubagentStop,
    ClaudeUserPromptSubmit, parse_permission_request, parse_post_compact, parse_post_tool_use,
    parse_pre_tool_use, parse_session_start, parse_stop, parse_stop_failure, parse_subagent_start,
    parse_subagent_stop, parse_user_prompt_submit,
};
use super::AskKind;
use super::RemoteControlStatus;
#[cfg(test)]
use super::StatusLineChange;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{
    BackgroundTask, HookRecord, SessionSource, decode_catalog_hook, hook_record,
};
use super::lifecycle::LifecycleSignal;
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentContext, AgentHookClass, AgentLifecycleObservation, AgentTurnError,
    DecodedHook, HookRouting, ManagedSource, Result, RootIdentity, SessionOrigin, SubagentIdentity,
    SubagentObservation, TranscriptMessage, non_empty_trimmed, optional_payload_string,
    read_transcript_tail, resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt,
    stop_payload_errored,
};
use crate::agents::TurnErrorClass;
use crate::transcript::AskQuestion;

/// Everything `const` about Claude Code, in one place. See
/// [`AgentDescriptor`] for the descriptor-vs-trait split.
static CLAUDE_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "claude",
    display_name: "Claude",
    brand: Brand {
        emblem: None,
        color: 173,
        color_rgb: (0xd9, 0x77, 0x57),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Claude" },
    // An Anthropic OAuth subscription is the account Claude meters, so a
    // multi-provider client (Pi) on that sub shares this budget.
    sub_providers: &["anthropic"],
    expected_windows: &["5h", "7d"],
    tools: ToolClassification {
        mutating: &["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"],
        editing: &["Edit", "Write", "MultiEdit", "NotebookEdit"],
        blocking: &[
            ("ExitPlanMode", AskKind::PlanApproval),
            ("AskUserQuestion", AskKind::Question),
        ],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: false,
        // Claude stamps a live pane on every session, so it opts out of the
        // lazy dead-stamp rebind. A genuinely paneless session can still be
        // recovered by cwd, and a pane with no session, such as the login
        // screen before SessionStart, is idle-synthesized like any wired agent.
        registers_lazily: false,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: true,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: true,
            background_sessions: true,
        },
    },
    coverage: CLAUDE_COVERAGE,
    lifecycle_hooks: CLAUDE_LIFECYCLE_HOOKS,
    default_context_window: Some(200_000),
    default_model: None,
    process_names: &["claude"],
    bin_names: &["claude"],
    extra_bin_dirs: &[],
    // A Claude session spreads across `<session_id>/chat.jsonl` plus
    // `<session_id>/subagents/*.jsonl`; the session directory is the thread.
    thread_key: ThreadKey::SessionDir,
    launch: super::LaunchSpec {
        program: Some("claude"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["claude", "--resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["claude", "--resume"],
            after_id: &["--fork-session"],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--permission-mode", "auto"],
            yolo: &["--dangerously-skip-permissions"],
            plan: &["--permission-mode", "plan"],
        },
        ping_args: Some(&["--model", "sonnet", "--effort", "low"]),
        max_turn_flag: Some("--max-turns"),
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: Some(super::StaticPresetMatcher::Flag(&["--system-prompt-file"])),
            append_system_prompt_file: Some(super::StaticPresetMatcher::Flag(&[
                "--append-system-prompt-file",
            ])),
        },
    },
};

const CLAUDE_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "PreToolUse:ExitPlanMode",
    },
    user_question: ConcernCoverage::Wired {
        via: "PreToolUse:AskUserQuestion",
    },
    answer: ConcernCoverage::Wired {
        via: "pane-native AskUserQuestion controls",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact/SessionStart:compact",
    },
    subagents: ConcernCoverage::Wired {
        via: "SubagentStart/SubagentStop/statusline",
    },
    background_parking: ConcernCoverage::Wired {
        via: "Stop.background_tasks/session_crons",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Wired {
        via: "Notification audit hook",
    },
    context_usage: ConcernCoverage::Wired {
        via: "transcript tail",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "statusline cost",
    },
    rich_context: ConcernCoverage::Wired { via: "statusline" },
    hook_install: ConcernCoverage::Wired {
        via: "~/.claude/settings.json",
    },
    account_spend: ConcernCoverage::Wired {
        via: "OAuth usage/transcripts",
    },
    remote_control: ConcernCoverage::Wired {
        via: "pane/background",
    },
};

const CLAUDE_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
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

/// Per-hook timeout written into the Claude config (seconds). Hooks write a
/// Waiting state and return neutral immediately, so the value is a short guard
/// for local I/O failures rather than an answer window.
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 10;

/// Installed events and classification policy. RimZ installs every event as a
/// single broad hook with no matcher: the helper classifies
/// each call from the payload's `tool_name`, so `PreToolUse: ExitPlanMode` and
/// `PreToolUse: AskUserQuestion` still route to their blocking ask kinds off
/// the broad `PreToolUse` hook. A dedicated `ExitPlanMode|AskUserQuestion`
/// matcher would only double-fire — Claude runs every matching matcher group,
/// and the broad entry already matches those tools. The broad
/// `PreToolUse`/`PostToolUse` hooks also keep the sidebar's enrichment current,
/// with their payload content gated by `[privacy] payload_mode`. The matcher
/// field stays explicit because the reclaim path still reasons about
/// on-disk matchers left by users or older builds.
const CLAUDE_HOOKS: &[HookRecord] = &[
    hook_record!(
        lifecycle,
        "SessionStart",
        r#"{"session_id":"sess-1","source":"startup"}"#
    )
    .progress(),
    hook_record!(lifecycle, "SessionEnd", r#"{"session_id":"sess-1"}"#).session_ended(),
    hook_record!(
        lifecycle,
        "UserPromptSubmit",
        r#"{"session_id":"sess-1","prompt":"fix auth"}"#
    )
    .progress(),
    hook_record!(lifecycle, "Stop", r#"{"session_id":"sess-1"}"#).progress(),
    hook_record!(
        lifecycle,
        "StopFailure",
        r#"{"session_id":"sess-1","error":"api_error"}"#
    ),
    hook_record!(lifecycle, "Notification", r#"{"session_id":"sess-1"}"#),
    hook_record!(
        blocking,
        "PermissionRequest",
        r#"{"session_id":"sess-1","tool_name":"Bash"}"#,
        AskKind::Permission
    )
    .synchronous()
    .with_lifecycle_fallback(),
    hook_record!(
        lifecycle,
        "PreToolUse",
        r#"{"session_id":"sess-1","tool_name":"Bash"}"#
    ),
    hook_record!(
        lifecycle,
        "PostToolUse",
        r#"{"session_id":"sess-1","tool_name":"Edit"}"#
    )
    .progress(),
    // Subagent lifecycle (Claude Code's Task-tool children, parity with Codex's
    // threads): `SubagentStart` registers a child row keyed by its `agent_id`,
    // `SubagentStop` returns it to idle. Both carry the parent root `session_id`.
    hook_record!(
        lifecycle,
        "SubagentStart",
        r#"{"session_id":"sess-parent","agent_id":"child-1","subagent_type":"Explore"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "SubagentStop",
        r#"{"session_id":"sess-parent","agent_id":"child-1","agent_type":"Explore"}"#
    )
    .progress(),
    // Fires around context compaction (manual `/compact` or auto). Pre opens
    // the transient compacting head; Post carries the trigger bit when present,
    // while SessionStart(source=compact) is the reliable triggerless closer.
    hook_record!(lifecycle, "PreCompact", r#"{"session_id":"sess-1"}"#),
    hook_record!(
        lifecycle,
        "PostCompact",
        r#"{"session_id":"sess-1","trigger":"manual"}"#
    ),
];

/// The exact command every rimz-managed Claude hook runs. Identical across all
/// events — the helper reads the event from the stdin payload's
/// `hook_event_name`, so no `--event` flag is needed.
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude";

/// Stable substring identifying a rimz-owned hook command across every form an
/// older build may have written (with `--event`, without `exec`). Used to
/// reclaim legacy and unmarked entries on install and uninstall, so duplicates
/// never accumulate.
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source claude";

/// `settings.json` key holding the statusline command Claude `exec`s on every
/// render. RimZ wraps it so it can capture the rich JSON Claude pipes there.
const STATUS_LINE_KEY: &str = "statusLine";
/// The statusline command RimZ installs. Fixed (no per-user content) so the
/// install stays idempotent and snapshot-stable; the wrapped original lives
/// under the shared managed wrapper marker, not embedded in this string.
const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude";
/// Stable substring identifying RimZ's own statusline reader across command
/// variants — and across both render commands, since the `subagentStatusLine`
/// command is a superstring of this. A statusline command matching this marker
/// is never a user command to wrap or pass through.
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source claude";

/// The session statusline: the rich per-render JSON blob Claude pipes for the
/// whole conversation.
const STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key_path: &[STATUS_LINE_KEY],
        command: STATUS_LINE_COMMAND,
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::All,
        wrap_policy: super::managed_statusline::WrapPolicy::Any,
        required_for_install: false,
    };

/// The per-child render command Claude `exec`s for each subagent row, carrying
/// the `tasks` array RimZ harvests. Wrapped the same way as the session
/// statusline; its command is the session reader plus `--subagent`.
const SUBAGENT_STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key_path: &["subagentStatusLine"],
        command: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude --subagent",
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::All,
        wrap_policy: super::managed_statusline::WrapPolicy::Any,
        required_for_install: false,
    };

#[derive(Clone, Debug, Default)]
pub struct ClaudeAdapter;

fn hook_ingress_decision(
    pid: Option<u32>,
    spawned_by_remote_control: bool,
) -> super::HookIngressDecision {
    if spawned_by_remote_control {
        super::HookIngressDecision::Ignore(super::HookIngressIgnoreReason::ClaudeRemoteControl)
    } else {
        super::HookIngressDecision::Accept(super::HookIngressOwner::agent(pid))
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CLAUDE_DESCRIPTOR
    }

    fn hook_ingress(&self, pid: Option<u32>) -> super::HookIngressDecision {
        hook_ingress_decision(pid, remote_control::spawned_by_remote_control())
    }

    #[cfg(test)]
    fn local_session_fixture(&self) -> Option<super::LocalSessionObservation> {
        Some(local_sessions::fixture_observation())
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<super::LocalSessionObservation> {
        local_sessions::discover(workspaces)
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<DecodedHook> {
        let parts = ClaudeLifecycleParts::parse(event_name, payload);
        // Cursor can execute Claude-compatible third-party hook commands with
        // Cursor-shaped payloads. Drop those before they can double-record or
        // be misparsed; `cursor_version` is Cursor's common-input discriminator.
        let mut decoded = if payload.get("cursor_version").is_some() {
            DecodedHook::new(super::ClassifiedHook {
                class: AgentHookClass::Unknown,
                ask_kind: None,
                event_name: event_name.to_owned(),
            })
        } else {
            let ask_kind = match event_name {
                "PermissionRequest" => self
                    .descriptor()
                    .blocking_tool_kind(
                        parts
                            .permission_request
                            .as_ref()
                            .and_then(|request| request.tool_name.as_deref()),
                    )
                    .is_none()
                    .then_some(AskKind::Permission),
                "PreToolUse" => self.descriptor().blocking_tool_kind(
                    parts
                        .pre_tool_use
                        .as_ref()
                        .and_then(|request| request.tool_name.as_deref()),
                ),
                _ => None,
            };
            decode_catalog_hook(CLAUDE_HOOKS, event_name, ask_kind)
        };
        decoded.set_routing(HookRouting::new(
            optional_payload_string(payload, &["agent_id", "session_id"]),
            optional_payload_string(payload, &["session_id", "agent_id"]),
            optional_payload_string(payload, &["worktree_path", "cwd"]),
            None,
        ));
        let questions = parts
            .pre_tool_use
            .as_ref()
            .and_then(|parsed| {
                ask::question_detail(parsed.tool_name.as_deref()?, parsed.tool_input.as_ref()?)
            })
            .unwrap_or_default();
        let ask_detail = if event_name == "PermissionRequest" {
            ask::permission_detail(payload)
        } else {
            questions
                .first()
                .and_then(|question| question.question.lines().next())
                .map(ToOwned::to_owned)
                .filter(|detail| !detail.is_empty())
        };
        decoded.set_ask(questions, ask_detail);
        decoded.set_native_answers(parts.post_tool_use.as_ref().and_then(|parsed| {
            ask::answer_detail(parsed.tool_name.as_deref()?, parsed.tool_response.as_ref()?)
        }));
        let terminal_tail = (event_name == "Stop")
            .then(|| transcript_tail_from_payload(payload))
            .flatten();
        let turn_error = parts
            .stop_failure
            .as_ref()
            .and_then(|parsed| {
                let error = parsed.error.as_deref()?.trim();
                if error.is_empty() {
                    return None;
                }
                let label = parsed
                    .last_assistant_message
                    .as_deref()
                    .and_then(statusline::cap_turn_error_label);
                let class = match error {
                    "rate_limit" => TurnErrorClass::PausedRateLimit,
                    "overloaded" => TurnErrorClass::PausedOverloaded,
                    _ => TurnErrorClass::classify_label(label.as_deref()),
                };
                Some(AgentTurnError {
                    class,
                    at: Timestamp::now(),
                    label,
                })
            })
            .or_else(|| {
                terminal_tail
                    .as_deref()
                    .and_then(statusline::detect_turn_error)
            });
        decoded.set_turn_error(turn_error);

        let signal = map_claude_lifecycle_signal(self.descriptor(), event_name, payload, &parts);
        if let Some(signal) = signal
            && let Some((agent_id, parent_agent_id)) = resolve_claude_observation_identity(
                self.descriptor().kind,
                event_name,
                payload,
                &parts,
            )
        {
            let mut observation =
                build_claude_observation(payload, &parts, signal, agent_id, parent_agent_id);
            if observation.parent_agent_id.is_none()
                && matches!(observation.signal, LifecycleSignal::Registered)
                && parts.session_start.as_ref().is_some_and(|start| {
                    matches!(start.source, SessionSource::Startup | SessionSource::Clear)
                })
            {
                observation.origin = Some(SessionOrigin::Fresh);
            }
            decoded.attach_lifecycle(observation);
        }
        let final_message = decoded.lifecycle().and_then(|observation| {
            final_message_for_lifecycle(payload, &observation, |path| {
                terminal_tail.or_else(|| read_transcript_tail(path))
            })
        });
        decoded.set_final_message(final_message);
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
        Ok(decoded)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        super::hook_types::catalog_event_names(CLAUDE_HOOKS)
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        let mut samples = super::hook_types::catalog_classification_corpus(CLAUDE_HOOKS);
        samples.extend([
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "AskUserQuestion" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "ExitPlanMode" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "ExitPlanMode" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::PlanApproval),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "AskUserQuestion" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
        ]);
        samples
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "sess-1",
            file_name: "chat.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"timestamp":"2026-06-02T10:00:00.000Z","sessionId":"sess-1","costUSD":0.42,"requestId":"req-1","message":{"id":"msg-1","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            ),
        })
    }

    fn ask_options(&self, kind: AskKind) -> Option<Vec<crate::transcript::AskOption>> {
        match kind {
            AskKind::Permission => Some(ask::permission_options()),
            AskKind::PlanApproval => Some(ask::plan_options()),
            AskKind::Question => None,
        }
    }

    fn answer_plan(
        &self,
        kind: AskKind,
        questions: &[AskQuestion],
        answers: &[super::AskReply],
    ) -> std::result::Result<Vec<super::AnswerStep>, super::AnswerPlanErr> {
        ask::answer_plan(kind, questions, answers)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        // Claude's transport is the statusline JSON blob. Tolerant parse: any
        // non-object payload yields `None` rather than an error.
        let parsed: statusline::StatuslinePayload = serde_json::from_value(payload.clone()).ok()?;
        let mut context = parsed.into_context(source, Timestamp::now());
        if let Some(tail) = transcript_tail_from_payload(payload) {
            context.turn_error = statusline::detect_turn_error(&tail);
            context.turn_interrupted = statusline::detect_turn_interrupted(&tail);
        }
        Some(context)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        statusline::parse_messages(lines)
    }

    fn observe_subagent_context(&self, payload: &Value) -> Vec<SubagentObservation> {
        // Claude's transport is the `subagentStatusLine` tasks array. Tolerant
        // parse: a non-object payload yields no observations rather than an error.
        let Ok(parsed) = serde_json::from_value::<subagent_statusline::SubagentStatuslinePayload>(
            payload.clone(),
        ) else {
            return Vec::new();
        };
        parsed.into_observations(Timestamp::now())
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&MANAGED_SOURCE)
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        oauth_usage::probe_usage(None)
    }

    fn remote_control_status(
        &self,
        account: Option<&crate::agents::AgentAccount>,
    ) -> RemoteControlStatus {
        let (_, settings) = remote_control::read_rc_settings();
        let version = account
            .and_then(|account| account.version.as_deref())
            .and_then(|version| version.parse().ok());
        RemoteControlStatus {
            pane_auto: remote_control::pane_auto_enabled(&settings, version),
        }
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::all_jsonl_files()
    }

    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        spend::claude_config_roots()
            .into_iter()
            .filter_map(|dir| {
                crate::agents::spending::SpendingSourceTree::new(dir.join("projects"), "**/*.jsonl")
            })
            .map(|tree| crate::agents::spending::SpendingSource::group(vec![tree]))
            .collect()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        let matches_session = |path: &Path| {
            path.components()
                .any(|component| component.as_os_str().to_string_lossy().contains(session_id))
        };
        let files: Vec<PathBuf> = self
            .transcript_files()
            .into_iter()
            .filter(|path| matches_session(path))
            .collect();
        files
            .iter()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("chat.jsonl"))
            .cloned()
            .or_else(|| files.into_iter().next())
    }

    /// Current Claude transcripts log no `costUSD`, so each turn is priced
    /// from its `message.usage` through the book; an older transcript's
    /// positive `costUSD` is used verbatim. Lines are independent, so a
    /// resume is a plain offset.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_claude_spend(path, resume.map_or(0, |cursor| cursor.offset), prices)
    }
}

struct ClaudeLifecycleParts {
    session_start: Option<ClaudeSessionStart>,
    user_prompt: Option<ClaudeUserPromptSubmit>,
    subagent_start: Option<ClaudeSubagentStart>,
    subagent_stop: Option<ClaudeSubagentStop>,
    stop: Option<ClaudeStop>,
    stop_failure: Option<ClaudeStopFailure>,
    pre_tool_use: Option<ClaudePreToolUse>,
    post_tool_use: Option<ClaudePostToolUse>,
    permission_request: Option<ClaudePermissionRequest>,
    post_compact: Option<ClaudePostCompact>,
    pending_background: Vec<String>,
}

impl ClaudeLifecycleParts {
    fn parse(event_name: &str, payload: &Value) -> Self {
        let session_start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let user_prompt =
            (event_name == "UserPromptSubmit").then(|| parse_user_prompt_submit(payload));
        let subagent_start = (event_name == "SubagentStart").then(|| parse_subagent_start(payload));
        let subagent_stop = (event_name == "SubagentStop").then(|| parse_subagent_stop(payload));
        let stop = (event_name == "Stop").then(|| parse_stop(payload));
        let stop_failure = (event_name == "StopFailure").then(|| parse_stop_failure(payload));
        let pre_tool_use = (event_name == "PreToolUse").then(|| parse_pre_tool_use(payload));
        let post_tool_use = (event_name == "PostToolUse").then(|| parse_post_tool_use(payload));
        let permission_request =
            (event_name == "PermissionRequest").then(|| parse_permission_request(payload));
        let post_compact = (event_name == "PostCompact").then(|| parse_post_compact(payload));
        let pending_background = stop
            .as_ref()
            .map(|p| pending_background_work(&p.background_tasks, &p.session_crons))
            .unwrap_or_default();
        Self {
            session_start,
            user_prompt,
            subagent_start,
            subagent_stop,
            stop,
            stop_failure,
            pre_tool_use,
            post_tool_use,
            permission_request,
            post_compact,
            pending_background,
        }
    }

    fn subagent_common(&self) -> Option<&ClaudeCommon> {
        self.subagent_start
            .as_ref()
            .map(|p| &p.common)
            .or_else(|| self.subagent_stop.as_ref().map(|p| &p.common))
    }
}

fn map_claude_lifecycle_signal(
    descriptor: &AgentDescriptor,
    event_name: &str,
    payload: &Value,
    parts: &ClaudeLifecycleParts,
) -> Option<LifecycleSignal> {
    match event_name {
        "SessionStart" => {
            let p = parts.session_start.as_ref()?;
            Some(match p.source {
                SessionSource::Compact => LifecycleSignal::CompactionEnded { auto: None },
                _ => LifecycleSignal::Registered,
            })
        }
        "UserPromptSubmit" => Some(LifecycleSignal::TurnStarted),
        "SubagentStart" => Some(LifecycleSignal::SubagentStarted),
        // The published SubagentStop payload has no outcome or exit-code
        // field, so close the bracket without inventing an error state.
        "SubagentStop" => Some(LifecycleSignal::SubagentStopped { errored: false }),
        "Stop" => Some(LifecycleSignal::TurnEnded {
            errored: stop_payload_errored(payload),
            parked_on_background: !parts.pending_background.is_empty(),
        }),
        "PermissionRequest" => descriptor
            .blocking_tool_kind(
                parts
                    .permission_request
                    .as_ref()
                    .and_then(|request| request.tool_name.as_deref()),
            )
            .is_none()
            .then_some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            }),
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: descriptor.tool_mutates(payload),
            edits: descriptor.tool_edits_files(payload),
            native_key: None,
        }),
        "PreToolUse" => {
            match descriptor.blocking_tool_kind(
                parts
                    .pre_tool_use
                    .as_ref()
                    .and_then(|request| request.tool_name.as_deref()),
            ) {
                Some(kind) => Some(LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                }),
                None => Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: None,
                }),
            }
        }
        "PreCompact" => Some(LifecycleSignal::Compacting),
        "PostCompact" => Some(LifecycleSignal::CompactionEnded {
            auto: parts
                .post_compact
                .as_ref()
                .and_then(|p| p.trigger.auto_flag()),
        }),
        "SessionEnd" => Some(LifecycleSignal::Ended),
        _ => None,
    }
}

type ObservationIdentity = (
    Option<crate::ids::AgentSessionId>,
    Option<crate::ids::AgentSessionId>,
);

fn resolve_claude_observation_identity(
    kind: &str,
    event_name: &str,
    payload: &Value,
    parts: &ClaudeLifecycleParts,
) -> Option<ObservationIdentity> {
    match parts.subagent_common() {
        Some(c) => match resolve_subagent_identity(
            kind,
            event_name,
            c.agent_id.as_deref(),
            c.common.session_id.as_deref(),
            payload,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => Some((Some(agent_id), Some(parent_agent_id))),
            SubagentIdentity::Quarantined => None,
        },
        None => match resolve_root_identity(
            kind,
            event_name,
            optional_payload_string(payload, &["agent_id"]).as_deref(),
            optional_payload_string(payload, &["session_id"]).as_deref(),
        ) {
            RootIdentity::Root { agent_id } => Some((agent_id, None)),
            RootIdentity::ForeignChild => None,
        },
    }
}

fn build_claude_observation(
    payload: &Value,
    parts: &ClaudeLifecycleParts,
    signal: LifecycleSignal,
    agent_id: Option<crate::ids::AgentSessionId>,
    parent_agent_id: Option<crate::ids::AgentSessionId>,
) -> AgentLifecycleObservation {
    let transcript_path = optional_payload_string(payload, &["session_id"])
        .and_then(|_| optional_payload_string(payload, &["transcript_path"]));
    let usage = transcript_path
        .as_deref()
        .map(usage_from_transcript)
        .unwrap_or_default();
    let payload_model = parts
        .session_start
        .as_ref()
        .and_then(|p| p.common.model.clone())
        .or_else(|| optional_payload_string(payload, &["model"]));
    let model = payload_model.clone().or(usage.model);
    // Assert a window only when the `[1m]` marker is actually present; a
    // marker-less hook leaves it `None` so the established window carries
    // forward. The gauge percentage is derived downstream from the folded
    // window, never baked here against a guessed denominator.
    let context_window = context_window_for(model.as_deref());
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    observation.parent_agent_id = parent_agent_id;
    observation.task = claude_task(payload, parts.subagent_common());
    observation.prompt =
        sanitize_user_prompt(parts.user_prompt.as_ref().and_then(|p| p.prompt.as_deref()));
    observation.transcript_path = transcript_path;
    observation.launch.model = model;
    observation.launch.effort = claude_effort(payload, parts);
    observation.usage.context_window = context_window;
    observation.usage.total_tokens = payload_total_tokens(payload, usage.total_tokens);
    observation
}

fn final_message_for_lifecycle(
    payload: &Value,
    observation: &AgentLifecycleObservation,
    read_tail: impl FnOnce(&Path) -> Option<String>,
) -> Option<String> {
    let needs_terminal_message =
        crate::harness::run::terminal_status_for_signal(&observation.signal).is_some();
    let needs_conversation_message = observation.parent_agent_id.is_none()
        && matches!(
            observation.signal,
            LifecycleSignal::TurnEnded { .. } | LifecycleSignal::AwaitingInput { .. }
        );
    if !needs_terminal_message && !needs_conversation_message {
        return None;
    }

    optional_payload_string(payload, &["last_assistant_message", "assistant_message"])
        .as_deref()
        .and_then(non_empty_trimmed)
        .or_else(|| {
            let path = observation
                .transcript_path
                .as_deref()
                .or_else(|| payload.get("transcript_path").and_then(Value::as_str))?;
            let tail = read_tail(Path::new(path))?;
            statusline::last_assistant_message(&tail)
        })
}

fn transcript_tail_from_payload(payload: &Value) -> Option<String> {
    let path = optional_payload_string(payload, &["transcript_path"])?;
    read_transcript_tail(Path::new(&path))
}

fn claude_task(payload: &Value, subagent_common: Option<&ClaudeCommon>) -> Option<String> {
    match subagent_common {
        Some(c) => c.agent_type.clone().or_else(|| {
            optional_payload_string(payload, &["subagent_type", "description", "task"])
        }),
        None => {
            sanitize_user_prompt(optional_payload_string(payload, &["task", "prompt"]).as_deref())
        }
    }
}

fn claude_effort(payload: &Value, parts: &ClaudeLifecycleParts) -> Option<String> {
    parts
        .stop
        .as_ref()
        .and_then(|p| p.common.effort.as_ref())
        .or_else(|| {
            parts
                .subagent_stop
                .as_ref()
                .and_then(|p| p.common.effort.as_ref())
        })
        .and_then(|e| e.level.clone())
        .or_else(|| optional_payload_string(payload, &["thinking_level"]))
}

/// Pending work from a typed Claude `Stop` payload (`background_tasks` and
/// `session_crons`, Claude Code v2.1.145+), as display labels. Pending work
/// means the main thread parked and will reawaken, so the row stays live.
/// Terminal background entries are skipped; every scheduled wakeup remains
/// pending until Claude removes it from the array. Older builds omit both
/// fields, which degrades to a genuine turn end through `Vec::default()`.
fn pending_background_work(
    tasks: &[BackgroundTask],
    crons: &[payloads::SessionCron],
) -> Vec<String> {
    let mut pending = tasks
        .iter()
        .filter(|task| {
            task.status
                .as_deref()
                .is_none_or(|status| !matches!(status, "completed" | "failed"))
        })
        .map(|task| {
            [&task.description, &task.command, &task.id]
                .into_iter()
                .find_map(|opt| opt.as_deref().filter(|label| !label.is_empty()))
                .unwrap_or("background task")
                .to_owned()
        })
        .collect::<Vec<_>>();
    pending.extend(crons.iter().map(|cron| {
        [&cron.prompt, &cron.schedule, &cron.id]
            .into_iter()
            .find_map(|opt| opt.as_deref().filter(|label| !label.is_empty()))
            .unwrap_or("scheduled wakeup")
            .to_owned()
    }));
    pending
}

/// Context-window usage derived from a Claude transcript tail. Carries the
/// latest turn's token total (context-occupying input plus output), the gauge
/// numerator the fold scales against the resolved window.
#[derive(Default)]
struct TranscriptUsage {
    total_tokens: Option<u64>,
    model: Option<String>,
}

impl TranscriptUsage {
    /// A transcript that opened cleanly but carries no assistant usage yet — a
    /// brand-new session. Report an explicit zero so the gauge draws an empty
    /// bar at 0% instead of vanishing until the first turn completes. A
    /// transcript that cannot be read stays `default()` (all `None`): unknown,
    /// not zero.
    fn fresh() -> Self {
        Self {
            total_tokens: Some(0),
            model: None,
        }
    }
}

/// The 1M-token context window when the model id carries the `[1m]` beta marker
/// (`claude-opus-4-8[1m]`), else `None` — a bare id is *unknown*, not 200k. The
/// marker rides only the hook payload's `model` field (the transcript always
/// writes the bare id), so a marker-less hook cannot distinguish a true 200k
/// model from a 1M model whose payload dropped the marker. Returning `None`
/// keeps the last established window (and the 200k descriptor default applies
/// when none was ever seen), so the gauge never downgrades a 1M agent to 200k.
fn context_window_for(model: Option<&str>) -> Option<u64> {
    const EXTENDED: u64 = 1_000_000;
    model
        .filter(|model| model.contains("[1m]"))
        .map(|_| EXTENDED)
}

/// Derive context-window usage from the tail of a Claude transcript JSONL.
/// Claude never puts token counts in the hook payload — they live in the
/// transcript — so this is the only place the context gauge can be sourced.
/// Reads a bounded tail and takes the most recent assistant `message.usage`.
/// Best-effort: any IO or parse failure yields empty fields (enrichment, never
/// correctness).
fn usage_from_transcript(path: &str) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(Path::new(path)) else {
        return TranscriptUsage::default();
    };
    // Newest-first: the last assistant usage record wins. A truncated leading
    // line from the tail seek simply fails to parse and is skipped.
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = value.get("message");
        let Some(usage) = message.and_then(|m| m.get("usage")) else {
            continue;
        };
        let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let context_tokens = field("input_tokens")
            + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens");
        let output = field("output_tokens");
        if context_tokens == 0 && output == 0 {
            continue;
        }
        let model = message
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned);
        // Raw tokens only: the window divisor is resolved downstream from the
        // folded window, which carries the `[1m]`-marked model's bump.
        return TranscriptUsage {
            total_tokens: Some(context_tokens + output),
            model,
        };
    }
    TranscriptUsage::fresh()
}

#[cfg(test)]
mod tests;
