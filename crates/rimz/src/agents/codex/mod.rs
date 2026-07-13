//! Codex hook adapter.
//!
//! Classifies `PermissionRequest` and blocking `PreToolUse` questions
//! (`request_user_input`) onto Waiting, plus the lifecycle events
//! (`SessionStart` registers idle, `SubagentStart` / `UserPromptSubmit` move
//! to running, `SubagentStop` returns the child to idle, `Stop` completes the
//! root turn — success, or failed on an error signal); neutral hook output is
//! empty stdout.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.codex/config.toml` using Codex's inline `[[hooks.Event]]` tables.
//!
//! Realtime details split across two sources. Usage (the context window, raw
//! token totals, token composition, and cost) is read from the rollout tail
//! through [`refresh_transcript_context`], because the Codex app-server exposes
//! token usage only on a live, subscribing `thread/resume` — never read-only.
//! The adapter emits raw tokens and the window, not a baked percentage; the
//! snapshot fold derives the gauge percentage from them.
//! The rollout head also feeds [`session_origin`], which lets the sidebar reap a
//! superseded same-pane session after `/clear` / `/new` without confusing a fork
//! for a replacement.
//! Metadata Claude gets from its statusline (rate-limit windows, model display
//! name, thread preview/name, version) comes from the app-server read-only
//! methods via [`refresh_app_server_context`], spawned out-of-band by `rimz codex
//! refresh-context`.

pub(crate) mod account;
pub(crate) mod app_server;
pub mod broker;
mod install;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub mod process;
pub(crate) mod spend;
mod transcript;

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use jiff::Timestamp;

use self::app_server::CodexAppServer;
use self::install::{
    codex_config_path, hooks_installed_at, install_into, managed_artifacts_at, preview_install_at,
    uninstall_from, untrusted_hook_events_at,
};
#[cfg(test)]
use self::install::{has_rimz_hook_command, snake_event_token};
use self::payloads::{
    CodexPermissionRequest, CodexPostCompact, CodexPreToolUse, CodexSessionStart,
    CodexSubagentStart, CodexSubagentStop, CodexUserPromptSubmit, parse_permission_request,
    parse_post_compact, parse_pre_tool_use, parse_session_start, parse_stop, parse_subagent_start,
    parse_subagent_stop, parse_user_prompt_submit,
};
pub(crate) use self::process::is_codex_cli_cmdline;
pub use self::process::{
    codex_daemon_pids, codex_resumed_session_id_for_root, codex_resumed_session_id_from_cmdline,
    pid_is_codex_daemon,
};
pub(crate) use self::transcript::infer_turn_death_from_spent_window;
use self::transcript::{
    TranscriptUsage, configured_model, configured_reasoning_effort, detect_turn_error,
    find_session_transcript, payload_reasoning_effort, usage_from_transcript_tail,
};
#[cfg(test)]
use self::transcript::{
    configured_model_at, configured_reasoning_effort_at, death_warning_from_frame,
    detect_turn_complete, detect_turn_interrupted, find_session_transcript_under,
    transcript_enrichment, transcript_stat, usage_from_transcript, with_codex_config_path,
    with_codex_sessions_root,
};
pub use self::transcript::{
    refine_turn_death_from_frame, refresh_transcript_context, session_origin,
    turn_death_needs_pane_confirmation,
};
use super::AskKind;
use super::context::AgentContext;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::hook_types::SessionSource;
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentLifecycleObservation, AgentTurnError, ClassifiedHook, ExtraCredits,
    HookInstallPreview, HookInstallReport, HookUninstallReport, LifecycleRefreshCtx,
    LocalContextRefresh, LocalContextRefreshCtx, RealtimeAccountUsage, RefreshSpawn,
    RefreshTrigger, ResetCredits, Result, RootIdentity, SubagentIdentity, TranscriptMessage,
    TranscriptRole, classify_agent_hook, non_empty_trimmed, optional_payload_string,
    read_transcript_tail, resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt,
    stop_payload_errored,
};
use crate::harness::run::PermissionMode;
use crate::transcript::AskQuestion;

/// Per-hook timeout written into the Codex config (seconds). Hooks write a
/// Waiting state and return neutral immediately, so the value is a short guard
/// for local I/O failures rather than an answer window.
const CODEX_HOOK_TIMEOUT_SECS: i64 = 10;

/// Codex's GPT-5.5 backend input ceiling — the observed 272k-token limit above
/// which the Codex backend rejects a prompt, listed by litellm and models.dev
/// as the Codex-family `max_input_tokens` / `limit.input`. The rollout's
/// `model_context_window` — Codex's effective window after its internal headroom
/// (`258_400 = 272k × 95%`) — replaces this as soon as it appears; until then the
/// agent card uses this stable provider fallback instead of briefly omitting the
/// window token.
const DEFAULT_CONTEXT_WINDOW: u64 = 272_000;
/// Valid Codex `--model` default stamped by `rimz agents` when no launch model
/// is configured. Adapter conformance pins this to Codex's shipped default.
const DEFAULT_MODEL: &str = "gpt-5.5-codex";

/// Marker Rimz sets on every `codex app-server` it spawns for read-only
/// enrichment (the cold-spawn in [`app_server`] and the warm [`broker`]). Such a
/// server is not a user session, yet Codex still fires its configured lifecycle
/// hooks (e.g. `SessionStart`) when it starts. Those hook children inherit this
/// marker, and `rimz hooks feed` no-ops on it — which breaks the
/// `refresh-context → cold-spawn app-server → SessionStart hook →
/// context_refresh_spawn → refresh-context` recursion that would otherwise
/// spawn unboundedly. Empty value means unset.
pub const ENV_INTERNAL_APP_SERVER: &str = "RIMZ_CODEX_INTERNAL_APP_SERVER";

/// True when the current process was spawned as a Rimz-internal enrichment
/// `codex app-server` (the [`ENV_INTERNAL_APP_SERVER`] marker is present and
/// non-empty). The hook entrypoint reads this to suppress re-entrant feeds.
pub fn spawned_as_internal_app_server() -> bool {
    std::env::var_os(ENV_INTERNAL_APP_SERVER).is_some_and(|value| !value.is_empty())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexQuestionInput {
    questions: Vec<CodexQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexQuestion {
    question: Option<String>,
}

fn codex_question_detail(tool_name: &str, tool_input: &Value) -> Option<Vec<AskQuestion>> {
    if tool_name != "request_user_input" {
        return None;
    }
    let parsed: CodexQuestionInput = serde_json::from_value(tool_input.clone()).ok()?;
    let questions = parsed
        .questions
        .into_iter()
        .filter_map(|question| {
            question
                .question
                .as_deref()
                .and_then(non_empty_trimmed)
                .map(|question| AskQuestion {
                    question,
                    options: Vec::new(),
                    multi_select: false,
                    has_option_previews: false,
                })
        })
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

/// Everything `const` about Codex, in one place. See [`AgentDescriptor`] for
/// the descriptor-vs-trait split.
static CODEX_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "codex",
    display_name: "Codex",
    brand: Brand {
        emblem: None,
        color: 38,
        color_rgb: (0x2f, 0xb1, 0xd1),
    },
    plan_label: PlanLabel::Prefixed { prefix: "ChatGPT" },
    // An OpenAI OAuth subscription is the ChatGPT account Codex meters; Pi's
    // auth file names it `openai` (legacy installs `openai-codex`).
    sub_providers: &["openai", "openai-codex"],
    tools: ToolClassification {
        mutating: &[
            "Bash",
            "shell",
            "apply_patch",
            "exec_command",
            "local_shell",
        ],
        editing: &["apply_patch"],
        // Codex's native blocking question tool is `request_user_input`.
        // Local rollout corpus on 2026-06-14 (Codex 0.139.0) contained 37
        // real function calls with this name and no `AskUserQuestion` or
        // `ExitPlanMode` calls. Re-verify against a teed `PreToolUse` stdin
        // before renaming this hook vocabulary.
        blocking: &[("request_user_input", AskKind::Question)],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: true,
        transcript_tail_context: true,
        context_usage: true,
        account_spend: true,
        subagents: true,
        // Codex has no background-task parking.
        background_tasks: false,
        // Codex fires no `SessionStart` on a plain CLI launch — it rides the
        // first `UserPromptSubmit` — and its hooks fire from the app-server
        // with no mux pane env, so a session is unstamped. Both make a Codex
        // instance present before any session binds: the sidebar binds it to
        // its pane by cwd and renders a wired-but-unprompted `codex` pane as
        // an idle agent.
        registers_lazily: true,
        daemon_hooked_sessions: true,
        hook_install: true,
        implicit_unlimited_window_mins: &[5 * 60],
        realtime_usage: RealtimeUsageChannel {
            covers_account_while_live: true,
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: true,
            background_sessions: true,
        },
    },
    coverage: CODEX_COVERAGE,
    lifecycle_hooks: CODEX_LIFECYCLE_HOOKS,
    default_context_window: Some(DEFAULT_CONTEXT_WINDOW),
    default_model: Some(DEFAULT_MODEL),
    // Codex commonly runs as a `node` bundle, so PID attribution accepts the
    // launcher process name beside its own.
    process_names: &["codex", "node"],
    bin_names: &["codex"],
    extra_bin_dirs: &[],
    // Codex hooks ride Claude-style event names; `PreToolUse` (races the
    // blocking ask) and `Notification` (idle) are deliberately absent.
    activity_events: &[
        "PostToolUse",
        "Stop",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    // Codex logs one rollout file per session.
    thread_key: ThreadKey::PerFile,
};

const CODEX_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
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
        ConcernCoverage::Unsupported {
            reason: "no plan-approval gate; update_plan is non-blocking",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "PreToolUse:request_user_input",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native prompt choreography is not mapped",
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
        ConcernCoverage::Unsupported {
            reason: "no background-task parking",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Partial {
            via: "pane liveness + rollup reaper",
            gap: "no SessionEnd hook; cleared on a snapshot tick, not at session exit",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn-end + request_user_input + stall window",
            gap: "no idle Notification hook; no idle-timeout nudge",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "rollout tail",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Wired {
            via: "rollout tail",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Wired { via: "app-server" },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.codex/config.toml",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Wired {
            via: "app-server/OAuth usage/rollouts",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Wired {
            via: "pane/background",
        },
    ),
];

const CODEX_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
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
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "no SessionEnd hook; cleared on a snapshot tick, not at session exit",
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

/// Installed events. Tuple is `(event_name, optional_matcher)` — the single
/// source of truth for which Codex events Rimz wires and with which matcher,
/// mirroring the Claude adapter's table. `SessionStart` filters to its
/// lifecycle subtypes; the per-call hooks match everything (`.*`); the
/// turn-boundary events (`UserPromptSubmit`, `Stop`) carry no matcher.
/// `UserPromptSubmit` is state signal — it moves the root agent to running and
/// carries the task. The broad `PreToolUse`/`PostToolUse` hooks fire on every
/// tool call; they keep the sidebar's enrichment current, with their payload
/// content gated by `[privacy] payload_mode`.
const INSTALLED_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear|compact")),
    ("UserPromptSubmit", None),
    ("SubagentStart", Some(".*")),
    ("SubagentStop", Some(".*")),
    ("Stop", None),
    ("PermissionRequest", Some(".*")),
    ("PreToolUse", Some(".*")),
    ("PostToolUse", Some(".*")),
    ("PreCompact", Some(".*")),
    ("PostCompact", Some(".*")),
];

const LIFECYCLE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "PreToolUse",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
];

/// Legacy config block written by older Rimz builds. Codex ignores this block;
/// uninstall still removes it so users can clean up stale config.
const RIMZ_BLOCK: &str = "rimz";
const HOOKS_TABLE: &str = "hooks";

/// The exact command every rimz-managed Codex hook runs. Identical across all
/// events — the helper reads the event from the stdin payload's
/// `hook_event_name`, so no `--event` flag is needed.
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex";

/// Stable substring identifying a rimz-owned hook command across every form an
/// older build may have written (with `--event`, without `exec`). Used to
/// reclaim legacy entries on install and uninstall, so duplicates never
/// accumulate.
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source codex";

#[derive(Clone, Debug, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CODEX_DESCRIPTOR
    }

    fn default_launch_model(&self) -> Option<String> {
        configured_model().or_else(|| self.descriptor().default_model.map(ToOwned::to_owned))
    }

    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (configured_model(), configured_reasoning_effort())
    }

    /// `codex resume <id>` resolves the UUID to its rollout file and restores
    /// the session interactively, firing `SessionStart` with
    /// `source: "resume"`. `resume` is a top-level command (the non-interactive
    /// form is `codex exec resume <id>`); the launching pane sets the cwd.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "codex".to_owned(),
            "resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn fork_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "codex".to_owned(),
            "fork".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Yolo => vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
            PermissionMode::Auto => vec![
                "--ask-for-approval".to_owned(),
                "never".to_owned(),
                "--sandbox".to_owned(),
                "workspace-write".to_owned(),
            ],
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Plan => Vec::new(),
        }
    }

    fn ping_args(&self) -> Option<Vec<String>> {
        Some(vec![
            "-c".to_owned(),
            "model_reasoning_effort=low".to_owned(),
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
            argv.extend(["-c".to_owned(), format!("model_reasoning_effort={effort}")]);
        }
        if let Some(path) = preset.system_prompt_file.as_deref() {
            argv.extend([
                "-c".to_owned(),
                format!("model_instructions_file={}", path.to_string_lossy()),
            ]);
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "append-system-prompt-file",
            });
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        match field {
            super::PresetField::Model => Some(super::PresetArgMatcher::Flag(vec![
                "--model".to_owned(),
                "-m".to_owned(),
            ])),
            super::PresetField::Effort => Some(super::PresetArgMatcher::ConfigKey {
                flags: vec!["-c".to_owned(), "--config".to_owned()],
                key: "model_reasoning_effort".to_owned(),
            }),
            super::PresetField::SystemPromptFile => Some(super::PresetArgMatcher::ConfigKey {
                flags: vec!["-c".to_owned(), "--config".to_owned()],
                key: "model_instructions_file".to_owned(),
            }),
            super::PresetField::AppendSystemPromptFile => None,
        }
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        Some(super::positional_prompt_argv("codex", extra_args, prompt))
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let ask_kind = match event_name {
            "PermissionRequest" => Some(AskKind::Permission),
            "PreToolUse" => self
                .descriptor()
                .blocking_tool_kind(parse_pre_tool_use(payload).tool_name.as_deref()),
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

        vec![
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "shell" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "request_user_input" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "shell" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostToolUse",
                serde_json::json!({ "session_id": "sess-1", "tool_name": "apply_patch" }),
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
                "SubagentStart",
                serde_json::json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStop",
                serde_json::json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review"
                }),
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
                "PreCompact",
                serde_json::json!({ "session_id": "sess-1", "trigger": "manual" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostCompact",
                serde_json::json!({ "session_id": "sess-1", "trigger": "manual" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "sess-1",
            file_name: "rollout-2026-06-02T10-00-00-sess-1.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"timestamp":"2026-06-02T10:00:00.000Z","model":"gpt-5","usage":{"input_tokens":100,"output_tokens":50}}"#,
            ),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Codex permission hooks expect empty stdout on the neutral path —
        // the agent's own UI then asks the human. Per docs/internals/agents/model.md:
        // never emit `updatedInput` / `interrupt` for Codex permission hooks.
        Ok(None)
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "PreToolUse" {
            return None;
        }
        let parsed = parse_pre_tool_use(payload);
        codex_question_detail(parsed.tool_name.as_deref()?, parsed.tool_input.as_ref()?)
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // Same turn-boundary signal as Claude: a fresh prompt or the root Stop
        // means the agent is past any native prompt it raised mid-turn. A
        // SubagentStop is a child finishing, not the human answering, so it does
        // not clear the root's asks.
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parts = CodexLifecycleParts::parse(event_name, payload);
        let transcript = codex_transcript_observation(payload, event_name == "Stop");
        let signal = map_codex_lifecycle_signal(
            self.descriptor(),
            event_name,
            payload,
            &parts,
            transcript.turn_error.as_ref(),
        )?;
        let (agent_id, parent_agent_id) = resolve_codex_observation_identity(
            self.descriptor().kind,
            event_name,
            payload,
            &parts,
            &signal,
        )?;
        let root_identity_event = parent_agent_id.is_none()
            && matches!(
                signal,
                LifecycleSignal::Registered | LifecycleSignal::TurnStarted
            );
        let mut observation = build_codex_observation(
            payload,
            &parts,
            signal,
            agent_id,
            parent_agent_id,
            transcript,
        );
        if root_identity_event && let Some(agent_id) = observation.agent_id.as_ref() {
            observation.origin = session_origin(agent_id.as_str());
        }
        Some(observation)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        match event_name {
            "Stop" => parse_stop(payload)
                .last_assistant_message
                .as_deref()
                .and_then(non_empty_trimmed),
            _ => None,
        }
    }

    fn observe_turn_error(&self, payload: &Value) -> Option<AgentTurnError> {
        codex_payload_turn_error(payload)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        parse_messages(lines)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = codex_config_path()?;
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = codex_config_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = codex_config_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        codex_config_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        codex_config_path().is_ok_and(|path| managed_artifacts_at(&path))
    }

    fn untrusted_installed_hooks(&self) -> Vec<String> {
        codex_config_path()
            .map(|path| untrusted_hook_events_at(&path))
            .unwrap_or_default()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_oauth_usage(&self) -> crate::agents::OauthUsageProbe {
        crate::agents::credits::map_probe_snapshot(oauth_usage::fetch_usage(), "codex")
    }

    fn oauth_credentials_stamp(&self) -> Option<u64> {
        oauth_usage::credentials_stamp()
    }

    fn oauth_account_key(&self) -> Option<String> {
        oauth_usage::account_key()
    }

    /// Codex has no statusline, so app-server-owned metadata (rate-limit
    /// windows, model display name, thread preview/name, version) refreshes
    /// out-of-band on turn boundaries: `SessionStart` populates it early (rate
    /// limits + model need no thread); `UserPromptSubmit`/`Stop` keep it
    /// current. Per-tool events are excluded — an app-server spawn per tool call
    /// is too frequent. Local transcript usage has its own stat-gated inline
    /// refresh below.
    fn context_refresh_spawn(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(event_name, "SessionStart" | "UserPromptSubmit" | "Stop")
        {
            return None;
        }
        let mut args = vec![
            "codex".to_owned(),
            "refresh-context".to_owned(),
            "--session-id".to_owned(),
            ctx.agent_id.to_owned(),
            "--workspace-id".to_owned(),
            ctx.workspace_id.to_owned(),
        ];
        if let Some(model) = ctx.model_hint {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        Some(RefreshSpawn { args })
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(
                event_name,
                "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop"
            )
        {
            return None;
        }
        refresh_transcript_context(
            ctx.agent_id,
            ctx.model_hint,
            ctx.prior_transcript_path,
            ctx.prior_transcript_stat,
            ctx.shared_pricing_cache_path,
        )
    }

    fn probe_realtime_account_usage(
        &self,
        runtime: &crate::RuntimePaths,
    ) -> Option<RealtimeAccountUsage> {
        refresh_app_server_enrichment(None, None, Some(&runtime.codex_app_server_socket_path()))
            .map(|enrichment| RealtimeAccountUsage {
                rate_limits: enrichment.context.rate_limits,
                extra_credits: enrichment.extra_credits,
                reset_credits: enrichment.reset_credits,
            })
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::codex_session_files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        find_session_transcript(session_id)
    }

    /// Codex logs token counts, not dollars — each event is multiplied
    /// through the price book. The resume cursor carries the cumulative-total
    /// and tracked-model fold state, so a suffix parse subtracts exactly.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_codex_spend(path, resume, prices)
    }
}

struct CodexLifecycleParts {
    session_start: Option<CodexSessionStart>,
    user_prompt: Option<CodexUserPromptSubmit>,
    subagent_start: Option<CodexSubagentStart>,
    subagent_stop: Option<CodexSubagentStop>,
    pre_tool_use: Option<CodexPreToolUse>,
    permission_request: Option<CodexPermissionRequest>,
    post_compact: Option<CodexPostCompact>,
}

type CodexSubagent<'a> = (&'a Option<String>, &'a Option<String>, &'a Option<String>);

fn codex_child_event<'a>(
    agent_id: &'a Option<String>,
    agent_type: &'a Option<String>,
    session_id: &'a Option<String>,
) -> Option<CodexSubagent<'a>> {
    let child = agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let parent = session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    (child != parent).then_some((agent_id, agent_type, session_id))
}

impl CodexLifecycleParts {
    fn parse(event_name: &str, payload: &Value) -> Self {
        Self {
            session_start: (event_name == "SessionStart").then(|| parse_session_start(payload)),
            user_prompt: (event_name == "UserPromptSubmit")
                .then(|| parse_user_prompt_submit(payload)),
            subagent_start: (event_name == "SubagentStart").then(|| parse_subagent_start(payload)),
            subagent_stop: (event_name == "SubagentStop").then(|| parse_subagent_stop(payload)),
            pre_tool_use: (event_name == "PreToolUse").then(|| parse_pre_tool_use(payload)),
            permission_request: (event_name == "PermissionRequest")
                .then(|| parse_permission_request(payload)),
            post_compact: (event_name == "PostCompact").then(|| parse_post_compact(payload)),
        }
    }

    fn subagent_for_signal(&self, signal: &LifecycleSignal) -> Option<CodexSubagent<'_>> {
        self.subagent_start
            .as_ref()
            .map(|p| (&p.agent_id, &p.agent_type, &p.common.common.session_id))
            .or_else(|| {
                self.subagent_stop
                    .as_ref()
                    .map(|p| (&p.agent_id, &p.agent_type, &p.common.common.session_id))
            })
            .or_else(|| {
                self.permission_request.as_ref().and_then(|p| {
                    codex_child_event(&p.agent_id, &p.agent_type, &p.common.common.session_id)
                })
            })
            .or_else(|| match signal {
                LifecycleSignal::AwaitingInput { .. } => self.pre_tool_use.as_ref().and_then(|p| {
                    codex_child_event(&p.agent_id, &p.agent_type, &p.common.common.session_id)
                }),
                _ => None,
            })
    }
}

fn map_codex_lifecycle_signal(
    descriptor: &AgentDescriptor,
    event_name: &str,
    payload: &Value,
    parts: &CodexLifecycleParts,
    turn_error: Option<&AgentTurnError>,
) -> Option<LifecycleSignal> {
    match event_name {
        "SessionStart" => {
            let p = parts.session_start.as_ref()?;
            Some(match p.source {
                SessionSource::Compact => LifecycleSignal::CompactionEnded { auto: None },
                _ => LifecycleSignal::Registered,
            })
        }
        "SubagentStart" => Some(LifecycleSignal::SubagentStarted),
        "UserPromptSubmit" => Some(LifecycleSignal::TurnStarted),
        "SubagentStop" => Some(LifecycleSignal::SubagentStopped { errored: false }),
        "Stop" => Some(LifecycleSignal::TurnEnded {
            errored: stop_payload_errored(payload) || turn_error.is_some(),
            parked_on_background: false,
        }),
        "PermissionRequest" => Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
        }),
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: descriptor.tool_mutates(payload),
            edits: descriptor.tool_edits_files(payload),
        }),
        "PreToolUse" => {
            match descriptor.blocking_tool_kind(
                parts
                    .pre_tool_use
                    .as_ref()
                    .and_then(|p| p.tool_name.as_deref()),
            ) {
                Some(kind) => Some(LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                }),
                None => Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
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
        _ => None,
    }
}

fn codex_payload_turn_error(payload: &Value) -> Option<AgentTurnError> {
    let session_id = optional_payload_string(payload, &["session_id"])?;
    let transcript_path = find_session_transcript(&session_id)?;
    let tail = read_transcript_tail(&transcript_path)?;
    detect_turn_error(&tail)
}

struct CodexTranscriptObservation {
    path: Option<PathBuf>,
    usage: TranscriptUsage,
    turn_error: Option<AgentTurnError>,
}

fn codex_transcript_observation(
    payload: &Value,
    detect_turn_death: bool,
) -> CodexTranscriptObservation {
    let path = optional_payload_string(payload, &["session_id"])
        .and_then(|id| find_session_transcript(&id));
    let tail = path.as_deref().and_then(read_transcript_tail);
    let usage = tail
        .as_deref()
        .map(usage_from_transcript_tail)
        .unwrap_or_default();
    let turn_error = detect_turn_death
        .then(|| tail.as_deref().and_then(detect_turn_error))
        .flatten();
    CodexTranscriptObservation {
        path,
        usage,
        turn_error,
    }
}

type ObservationIdentity = (
    Option<crate::ids::AgentSessionId>,
    Option<crate::ids::AgentSessionId>,
);

fn resolve_codex_observation_identity(
    kind: &str,
    event_name: &str,
    payload: &Value,
    parts: &CodexLifecycleParts,
    signal: &LifecycleSignal,
) -> Option<ObservationIdentity> {
    match parts.subagent_for_signal(signal) {
        Some((child, _, parent)) => match resolve_subagent_identity(
            kind,
            event_name,
            child.as_deref(),
            parent.as_deref(),
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

fn build_codex_observation(
    payload: &Value,
    parts: &CodexLifecycleParts,
    signal: LifecycleSignal,
    agent_id: Option<crate::ids::AgentSessionId>,
    parent_agent_id: Option<crate::ids::AgentSessionId>,
    transcript: CodexTranscriptObservation,
) -> AgentLifecycleObservation {
    let usage = transcript.usage;
    let usage_effort = usage.effort.clone();
    let is_subagent = parent_agent_id.is_some();
    let subagent = parts.subagent_for_signal(&signal);
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    observation.parent_agent_id = parent_agent_id;
    observation.task = codex_task(payload, subagent);
    observation.prompt =
        sanitize_user_prompt(parts.user_prompt.as_ref().and_then(|p| p.prompt.as_deref()));
    observation.transcript_path = transcript
        .path
        .map(|path| path.to_string_lossy().into_owned());
    observation.turn_error = transcript.turn_error;
    let reported_context_window = usage.reported_context_window();
    observation.launch.model = optional_payload_string(payload, &["model"]).or(usage.model);
    observation.launch.effort = payload_reasoning_effort(payload)
        .or(usage_effort)
        .or_else(|| is_subagent.then(configured_reasoning_effort).flatten());
    observation.context_window = reported_context_window;
    observation.total_tokens = payload_total_tokens(payload, usage.total_tokens);
    observation.cache_read_input_tokens = usage.last_cached_input_tokens;
    observation.fresh_input_tokens = usage
        .last_input_tokens
        .map(|input| input.saturating_sub(usage.last_cached_input_tokens.unwrap_or(0)));
    observation.output_tokens = usage.last_output_tokens;
    observation
}

fn codex_task(payload: &Value, subagent: Option<CodexSubagent<'_>>) -> Option<String> {
    match subagent {
        Some((_, agent_type, _)) => agent_type.clone().or_else(|| {
            sanitize_user_prompt(optional_payload_string(payload, &["task", "prompt"]).as_deref())
        }),
        None => {
            sanitize_user_prompt(optional_payload_string(payload, &["task", "prompt"]).as_deref())
        }
    }
}

/// Read Codex's read-only realtime details from the app-server and project them
/// onto an [`AgentContext`] for the session sidecar. Spawned out-of-band by
/// `rimz codex refresh-context` (never inline in a hook). The app-server owns
/// rate-limit windows, account plan, model display name, thread preview/name,
/// and version.
/// Transcript-derived tokens and cost are refreshed separately from the local
/// rollout tail, so an unreachable app-server never suppresses them.
pub fn refresh_app_server_context(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<AgentContext> {
    refresh_app_server_enrichment(session_id, model_hint, broker_socket)
        .map(|enrichment| enrichment.context)
}

pub struct AppServerEnrichment {
    pub context: AgentContext,
    pub extra_credits: Option<ExtraCredits>,
    pub reset_credits: Option<ResetCredits>,
}

pub fn refresh_app_server_enrichment(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<AppServerEnrichment> {
    let mut client = CodexAppServer::connect(broker_socket)?;
    let observation = client.observe("codex", session_id, model_hint, Timestamp::now());
    Some(AppServerEnrichment {
        context: observation.context,
        extra_credits: observation.extra_credits,
        reset_credits: observation.reset_credits,
    })
}

/// Backwards-compatible name for the app-server-only context read. New callers
/// use [`refresh_app_server_context`] and [`refresh_transcript_context`] so local
/// transcript data is independent from app-server availability.
pub fn refresh_context(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<AgentContext> {
    refresh_app_server_context(session_id, model_hint, broker_socket)
}

/// The thread ids the per-user Codex app-server daemon currently holds in memory,
/// for the sidebar's daemon-mode ghost reap
/// ([`crate::store::snapshot::SidebarSnapshot::reap_runtime`]).
/// Connects to the daemon **specifically** — never a cold-spawn, whose empty set
/// would mass-reap — and reads `thread/loaded/list`. `None` when there is no daemon
/// to ask or its list cannot be trusted, which the caller reads as "unknown, keep
/// all". Spawned out-of-band by the sidebar producer; read-only, best-effort.
pub fn loaded_daemon_threads() -> Option<std::collections::BTreeSet<String>> {
    let mut client = CodexAppServer::connect_daemon()?;
    let ids = client.loaded_threads().ok()?;
    Some(ids.into_iter().collect())
}

fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let value = serde_json::from_str::<Value>(line).ok()?;
            if value.get("type").and_then(Value::as_str) != Some("event_msg") {
                return None;
            }
            let payload = value.get("payload")?;
            let role = match payload.get("type").and_then(Value::as_str) {
                Some("user_message") => TranscriptRole::User,
                Some("agent_message") => TranscriptRole::Assistant,
                _ => return None,
            };
            payload
                .get("message")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .map(|text| TranscriptMessage {
                    role,
                    at: value
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(|raw| raw.parse().ok()),
                    text,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests;
