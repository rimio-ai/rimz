//! Claude Code hook adapter.
//!
//! Classifies the blocking events (`PermissionRequest`, `PreToolUse:
//! ExitPlanMode`, `PreToolUse: AskUserQuestion`) and the lifecycle events
//! (`SessionStart` registers idle, `UserPromptSubmit` moves to running with
//! the prompt as task, `Stop` completes the turn — success, or failed on an
//! error signal, or back to running when the payload's `background_tasks`
//! still has work in flight, `SessionEnd` exits, `Notification` silent);
//! renders the Claude-shaped `hookSpecificOutput` / `updatedInput` decision
//! payload and the silent neutral fallback. Context budget is read from the
//! transcript tail.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers. The
//! `PermissionRequest` blocking hook is marked `_rimz_sync = true`; an existing
//! async marker on it is a hard install error (see [`BLOCKING_EVENTS`] and
//! `docs/internals/agents/adapter/claude.md`). The `PreToolUse` blocking sub-events ride the
//! broad `PreToolUse` hook and self-classify from `tool_name`.

pub(crate) mod account;
mod ask;
mod install;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub(crate) mod remote_control;
pub(crate) mod spend;
mod statusline;
mod subagent_statusline;

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
#[cfg(test)]
use serde_json::Map;
use serde_json::Value;

#[cfg(test)]
use self::install::{classify_status_line_change, upsert_rimz_status_line};
use self::install::{
    claude_settings_path, hooks_installed_at, install_into, managed_artifacts_at,
    preview_install_at, read_existing_json, uninstall_from, wrapped_status_line_command_from,
};
use self::payloads::{
    ClaudeCommon, ClaudePermissionBehavior, ClaudePermissionDecisionOutput,
    ClaudePermissionHookOutput, ClaudePostCompact, ClaudePreToolUseDecisionOutput,
    ClaudePreToolUseHookOutput, ClaudeSessionStart, ClaudeStop, ClaudeSubagentStart,
    ClaudeSubagentStop, ClaudeUserPromptSubmit, parse_post_compact, parse_post_tool_use,
    parse_pre_tool_use, parse_session_start, parse_stop, parse_stop_failure, parse_subagent_start,
    parse_subagent_stop, parse_user_prompt_submit,
};
use super::RemoteControlStatus;
#[cfg(test)]
use super::StatusLineChange;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::hook_types::{BackgroundTask, SessionSource};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentContext, AgentErr, AgentLifecycleObservation, AgentTurnError, AskAnswer,
    AskQuestion, ClassifiedHook, HookInstallPreview, HookInstallReport, HookUninstallReport,
    Result, RootIdentity, SubagentIdentity, SubagentObservation, TranscriptMessage,
    choice_is_allow, classify_agent_hook, non_empty_trimmed, optional_payload_string,
    read_transcript_tail, resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt,
    stop_payload_errored,
};
use crate::agents::TurnErrorClass;
use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::run::PermissionMode;

/// Claude's effective hook cap. The upstream cap is ~125s; we leave a small
/// margin so the bridge never holds the hook past Claude's kill window.
const CLAUDE_HOOK_CAP: Duration = Duration::from_secs(120);

/// Everything `const` about Claude Code, in one place. See
/// [`AgentDescriptor`] for the descriptor-vs-trait split.
static CLAUDE_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "claude",
    display_name: "Claude",
    brand: Brand {
        emblem: "
 ▐▛███▜▌
▝▜█████▛▘
  ▘▘ ▝▝",
        color: 173,
        color_rgb: (0xd9, 0x77, 0x57),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Claude" },
    // An Anthropic OAuth subscription is the account Claude meters, so a
    // multi-provider client (Pi) on that sub shares this budget.
    sub_providers: &["anthropic"],
    tools: ToolClassification {
        mutating: &["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"],
        editing: &["Edit", "Write", "MultiEdit", "NotebookEdit"],
        blocking: &[
            ("ExitPlanMode", FeedKind::PlanApproval),
            ("AskUserQuestion", FeedKind::Question),
        ],
    },
    capabilities: Capabilities {
        blocking_feed: true,
        native_ask_ui: true,
        rich_context: true,
        context_usage: true,
        account_spend: true,
        subagents: true,
        background_tasks: true,
        // Claude stamps a live pane on every session, so a pane with no
        // session is not idle-synthesized. Read-time cwd recovery still
        // rebinds a live pane after a mux rebirth clears the stamp.
        registers_lazily: false,
        hook_install: true,
        remote_control: RemoteControlCapability {
            pane_sessions: true,
            background_sessions: true,
        },
    },
    coverage: CLAUDE_COVERAGE,
    lifecycle_hooks: CLAUDE_LIFECYCLE_HOOKS,
    default_context_window: Some(200_000),
    default_model: None,
    hook_cap: CLAUDE_HOOK_CAP,
    process_names: &["claude"],
    extra_bin_dirs: &[],
    // `PreToolUse` (races the blocking ask) and `Notification` (idle) are
    // deliberately absent.
    activity_events: &[
        "PostToolUse",
        "Stop",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    // A Claude session spreads across `<session_id>/chat.jsonl` plus
    // `<session_id>/subagents/*.jsonl`; the session directory is the thread.
    thread_key: ThreadKey::SessionDir,
};

const CLAUDE_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop",
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
            via: "PreToolUse:ExitPlanMode",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "PreToolUse:AskUserQuestion",
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
            via: "SubagentStart/SubagentStop/statusline",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Wired {
            via: "Stop.background_tasks",
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
            via: "transcript tail",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Wired {
            via: "statusline cost",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Wired { via: "statusline" },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.claude/settings.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Wired {
            via: "OAuth usage/transcripts",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Wired {
            via: "pane/background",
        },
    ),
];

const CLAUDE_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
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
];

/// Per-hook timeout written into the Claude config (seconds). Matches
/// [`CLAUDE_HOOK_CAP`] so the agent and bridge agree on the ceiling.
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 120;

/// Installed events. Tuple is `(event_name, optional_matcher)`. Rimz installs
/// every event as a single broad hook with no matcher: the helper classifies
/// each call from the payload's `tool_name`, so `PreToolUse: ExitPlanMode` and
/// `PreToolUse: AskUserQuestion` still route to their blocking feed kinds off
/// the broad `PreToolUse` hook. A dedicated `ExitPlanMode|AskUserQuestion`
/// matcher would only double-fire — Claude runs every matching matcher group,
/// and the broad entry already matches those tools. The broad
/// `PreToolUse`/`PostToolUse` hooks also keep the sidebar's enrichment current
/// and feed `rimz feed list --audit` depth, with their payload content gated by
/// `[privacy] payload_mode`. The matcher slot stays in the tuple because the
/// reclaim path still reasons about on-disk matchers left by users or older
/// builds.
const INSTALLED_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("SessionEnd", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("StopFailure", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
    // Subagent lifecycle (Claude Code's Task-tool children, parity with Codex's
    // threads): `SubagentStart` registers a child row keyed by its `agent_id`,
    // `SubagentStop` returns it to idle. Both carry the parent root `session_id`.
    ("SubagentStart", None),
    ("SubagentStop", None),
    // Fires around context compaction (manual `/compact` or auto). Pre opens
    // the transient compacting head; Post carries the trigger bit when present,
    // while SessionStart(source=compact) is the reliable triggerless closer.
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
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
];

/// Events that hold the agent open while the bridge waits for an answer.
/// Installing one with `_rimz_sync = false` in the existing config is a hard
/// error — the source of truth for "must block" is this constant, never the
/// on-disk file.
const BLOCKING_EVENTS: &[(&str, Option<&str>)] = &[("PermissionRequest", None)];

const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_SYNC_KEY: &str = "_rimz_sync";

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
/// render. Rimz wraps it so it can capture the rich JSON Claude pipes there.
const STATUS_LINE_KEY: &str = "statusLine";
/// Marker key, on a Rimz-managed `statusLine` object, holding the user's
/// original `statusLine` value verbatim so uninstall restores it exactly.
const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";
/// The statusline command Rimz installs. Fixed (no per-user content) so the
/// install stays idempotent and snapshot-stable; the wrapped original lives
/// under [`RIMZ_WRAPPED_KEY`], not embedded in this string.
const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude";
/// Stable substring identifying Rimz's own statusline reader across command
/// variants — and across both render commands, since the `subagentStatusLine`
/// command is a superstring of this. A statusline command matching this marker
/// is never a user command to wrap or pass through.
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source claude";

/// A statusline-style `settings.json` command Rimz wraps: the key it lives under
/// and the fixed reader command Rimz installs there. The wrap markers
/// ([`RIMZ_WRAPPED_KEY`], [`RIMZ_MANAGED_KEY`]) and the recursion guard
/// ([`RIMZ_STATUS_LINE_MARKER`], a substring of every Rimz reader command) are
/// shared, so one set of upsert/strip/classify logic serves every spec.
struct StatusLineSpec {
    key: &'static str,
    command: &'static str,
}

/// The session statusline: the rich per-render JSON blob Claude pipes for the
/// whole conversation.
const STATUS_LINE: StatusLineSpec = StatusLineSpec {
    key: STATUS_LINE_KEY,
    command: STATUS_LINE_COMMAND,
};

/// The per-child render command Claude `exec`s for each subagent row, carrying
/// the `tasks` array Rimz harvests. Wrapped the same way as the session
/// statusline; its command is the session reader plus `--subagent`.
const SUBAGENT_STATUS_LINE: StatusLineSpec = StatusLineSpec {
    key: "subagentStatusLine",
    command: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude --subagent",
};

#[derive(Clone, Debug, Default)]
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CLAUDE_DESCRIPTOR
    }

    /// `claude --resume <id>` launches straight into the prior session,
    /// restoring its conversation and firing `SessionStart` with
    /// `source: "resume"`. The cwd is set by the launching pane, not the argv.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "claude".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Auto => vec!["--permission-mode".to_owned(), "auto".to_owned()],
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Yolo => vec!["--dangerously-skip-permissions".to_owned()],
            PermissionMode::Plan => vec!["--permission-mode".to_owned(), "plan".to_owned()],
        }
    }

    fn ping_args(&self) -> Option<Vec<String>> {
        Some(vec![
            "--effort".to_owned(),
            "low".to_owned(),
            "ping".to_owned(),
        ])
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/compact")
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|model| !model.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|effort| !effort.is_empty()) {
            argv.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        if let Some(path) = preset.system_prompt_file.as_deref() {
            argv.extend([
                "--system-prompt-file".to_owned(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        if let Some(path) = preset.append_system_prompt_file.as_deref() {
            argv.extend([
                "--append-system-prompt-file".to_owned(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        Ok(argv)
    }

    fn max_turns_args(&self, limit: u32) -> Option<Vec<String>> {
        Some(vec!["--max-turns".to_owned(), limit.to_string()])
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["claude".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.push(prompt.to_owned());
        }
        Some(argv)
    }

    fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        // Claude Code ≥2.1.173 opens its agents dashboard by default; the
        // Rimz pane contract (hooks, transcript tail, message sends)
        // drives the classic interactive REPL.
        vec![(remote_control::DISABLE_AGENT_VIEW_ENV, "1")]
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let feed_kind = match event_name {
            "PermissionRequest" => Some(FeedKind::Permission),
            // ExitPlanMode / AskUserQuestion self-classify off the tool name on
            // the broad PreToolUse hook; every other tool call is plain lifecycle.
            "PreToolUse" => self
                .descriptor()
                .blocking_tool_kind(parse_pre_tool_use(payload).tool_name.as_deref()),
            _ => None,
        };

        classify_agent_hook(event_name, feed_kind, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        INSTALLED_EVENTS.iter().map(|(event, _)| *event).collect()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        vec![
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "Bash" }),
                AgentHookClass::BlockingFeed,
                Some(FeedKind::Permission),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "ExitPlanMode" }),
                AgentHookClass::BlockingFeed,
                Some(FeedKind::PlanApproval),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "AskUserQuestion" }),
                AgentHookClass::BlockingFeed,
                Some(FeedKind::Question),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "Bash" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "Edit" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SessionStart",
                serde_json::json!({ "session_id": "sess-1", "source": "startup" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "UserPromptSubmit",
                serde_json::json!({ "session_id": "sess-1", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Stop",
                serde_json::json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "StopFailure",
                serde_json::json!({ "session_id": "sess-1", "error": "api_error" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Notification",
                serde_json::json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStart",
                serde_json::json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-1",
                    "subagent_type": "Explore"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStop",
                serde_json::json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-1",
                    "agent_type": "Explore"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PreCompact",
                serde_json::json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostCompact",
                serde_json::json!({ "session_id": "sess-1", "trigger": "manual" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SessionEnd",
                serde_json::json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
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

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => {
                let output = ClaudePermissionDecisionOutput {
                    hook_specific_output: ClaudePermissionHookOutput {
                        hook_event_name: "PermissionRequest",
                        decision: ClaudePermissionBehavior {
                            behavior: if choice_is_allow(resolution) {
                                "allow"
                            } else {
                                "deny"
                            },
                            updated_input: None,
                            applied_rule: None,
                        },
                    },
                };
                Ok(serde_json::to_value(output)
                    .expect("ClaudePermissionDecisionOutput is infallible"))
            }
            FeedKind::PlanApproval | FeedKind::Question => {
                let updated_input = resolution
                    .decision
                    .get("updatedInput")
                    .or_else(|| resolution.decision.get("updated_input"))
                    .ok_or(AgentErr::MissingField {
                        agent: "claude",
                        field: "updatedInput",
                    })?
                    .clone();
                let output = ClaudePreToolUseDecisionOutput {
                    hook_specific_output: ClaudePreToolUseHookOutput {
                        hook_event_name: "PreToolUse",
                        permission_decision: if choice_is_allow(resolution) {
                            "allow"
                        } else {
                            "deny"
                        },
                        updated_input,
                    },
                };
                Ok(serde_json::to_value(output)
                    .expect("ClaudePreToolUseDecisionOutput is infallible"))
            }
            other => Err(AgentErr::Render {
                agent: "claude",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Claude treats stdout as a control/context surface. The safe no-op is
        // exit 0 with no stdout; only resolver decisions write JSON.
        Ok(None)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // A new prompt starts a fresh turn; a Stop ends the current one. Either
        // way the agent is past any native_ui ask it raised mid-turn — Claude's
        // *main thread* blocks on its own prompt and emits no events until the
        // human answers it, so by the time one of these arrives the ask is
        // settled in its UI. A backgrounded subagent does keep emitting while
        // the main thread blocks, but every in-subagent payload carries the
        // child `agent_id`, so expiry (keyed by `payload_agent_id`) scopes to
        // the child and the lifecycle channel drops the event entirely
        // (`resolve_root_identity`) — neither can settle the parent's ask.
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "PreToolUse" {
            return None;
        }
        let parsed = parse_pre_tool_use(payload);
        ask::question_detail(parsed.tool_name.as_deref()?, parsed.tool_input.as_ref()?)
    }

    fn native_ask_answer(&self, event_name: &str, payload: &Value) -> Option<Vec<AskAnswer>> {
        if event_name != "PostToolUse" {
            return None;
        }
        let parsed = parse_post_tool_use(payload);
        ask::answer_detail(parsed.tool_name.as_deref()?, parsed.tool_response.as_ref()?)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parts = ClaudeLifecycleParts::parse(event_name, payload);
        let signal = map_claude_lifecycle_signal(self.descriptor(), event_name, payload, &parts)?;
        let (agent_id, parent_agent_id) = resolve_claude_observation_identity(
            self.descriptor().kind,
            event_name,
            payload,
            &parts,
        )?;
        Some(build_claude_observation(
            payload,
            &parts,
            signal,
            agent_id,
            parent_agent_id,
        ))
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        // Claude's transport is the statusline JSON blob. Tolerant parse: any
        // non-object payload yields `None` rather than an error.
        let parsed: statusline::StatuslinePayload = serde_json::from_value(payload.clone()).ok()?;
        Some(parsed.into_context(source, Timestamp::now()))
    }

    fn observe_turn_error(&self, payload: &Value) -> Option<AgentTurnError> {
        // The statusline payload names the live transcript, and its tail is the
        // only record of an API-error abort — Claude fires no `Stop` for one
        // (docs/internals/agents/adapter/claude.md). Best-effort: an absent
        // path or unreadable file is `None`, never an error.
        let path = optional_payload_string(payload, &["transcript_path"])?;
        let tail = read_transcript_tail(Path::new(&path))?;
        statusline::detect_turn_error(&tail)
    }

    fn observe_turn_error_from_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentTurnError> {
        if event_name != "StopFailure" {
            return None;
        }
        let parsed = parse_stop_failure(payload);
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
            _ => statusline::classify_turn_error_label(label.as_deref()),
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
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        optional_payload_string(payload, &["last_assistant_message", "assistant_message"])
            .as_deref()
            .and_then(non_empty_trimmed)
            .or_else(|| {
                let path = observation
                    .transcript_path
                    .as_deref()
                    .or_else(|| payload.get("transcript_path").and_then(Value::as_str))?;
                let tail = read_transcript_tail(Path::new(path))?;
                statusline::last_assistant_message(&tail)
            })
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

    fn wrapped_status_line_command(&self) -> Option<String> {
        let path = claude_settings_path().ok()?;
        let root = read_existing_json(&path).ok()?;
        wrapped_status_line_command_from(&root, &STATUS_LINE)
    }

    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        let path = claude_settings_path().ok()?;
        let root = read_existing_json(&path).ok()?;
        wrapped_status_line_command_from(&root, &SUBAGENT_STATUS_LINE)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = claude_settings_path()?;
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = claude_settings_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = claude_settings_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        claude_settings_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        claude_settings_path().is_ok_and(|path| managed_artifacts_at(&path))
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_oauth_usage(&self) -> crate::agents::OauthUsageProbe {
        crate::agents::credits::map_probe_snapshot(
            oauth_usage::fetch_usage(None),
            "claude.oauth_usage",
        )
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
        let post_compact = (event_name == "PostCompact").then(|| parse_post_compact(payload));
        let pending_background = stop
            .as_ref()
            .map(|p| pending_background_tasks(&p.background_tasks))
            .unwrap_or_default();
        Self {
            session_start,
            user_prompt,
            subagent_start,
            subagent_stop,
            stop,
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
        "SubagentStop" => Some(LifecycleSignal::SubagentStopped {
            errored: parts
                .subagent_stop
                .as_ref()
                .and_then(|p| p.exit_code)
                .is_some_and(|code| code != 0),
        }),
        "Stop" => Some(LifecycleSignal::TurnEnded {
            errored: stop_payload_errored(payload),
            parked_on_background: !parts.pending_background.is_empty(),
        }),
        "PostToolUse" if descriptor.tool_mutates(payload) => Some(LifecycleSignal::ToolUsed {
            mutates: true,
            edits: descriptor.tool_edits_files(payload),
        }),
        "PreToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }),
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
    observation.model = model;
    observation.effort = claude_effort(payload, parts);
    observation.context_window = context_window;
    observation.total_tokens = payload_total_tokens(payload, usage.total_tokens);
    observation
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

/// In-flight background tasks from a typed Claude `Stop` payload
/// (`background_tasks`, Claude Code v2.1.145+), as display labels. A `Stop`
/// with pending background work is the main thread parking, not a turn end —
/// it reawakens when the work reports back — so the row must stay live. Each
/// in-flight entry's label is its `description`, else `command`, else `id`; an
/// entry with a terminal `status` (`completed`/`failed`) is no longer in
/// flight and is skipped. An all-terminal or empty slice yields an empty vec:
/// a genuine turn end. Older Claude builds omit the field entirely, which
/// degrades to the same empty vec via the typed struct's `Vec::default()`.
fn pending_background_tasks(tasks: &[BackgroundTask]) -> Vec<String> {
    tasks
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
        .collect()
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
