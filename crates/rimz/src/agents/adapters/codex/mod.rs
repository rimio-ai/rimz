//! Codex hook adapter.
//!
//! Classifies `PermissionRequest` and blocking `PreToolUse` questions
//! (`request_user_input`) onto Waiting, plus the lifecycle events
//! (`SessionStart` registers idle, prompt/tool/compaction hooks advance their
//! typed root or child, `SubagentStop` returns the child to idle, and `Stop`
//! either raises a rollout-derived plan ask or completes the root turn);
//! neutral hook output is empty stdout.
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
//! methods via [`refresh_app_server_enrichment`], spawned out-of-band by
//! `rimz agents refresh-context`.

pub(crate) mod account;
pub(crate) mod app_server;
mod ask;
pub mod broker;
mod install;
mod local_sessions;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub mod process;
mod rollout;
pub(crate) mod spend;
mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use serde_json::Value;

use jiff::Timestamp;

use self::app_server::CodexAppServer;
pub use self::app_server::{app_server_due, merge_app_server_context};
#[cfg(test)]
use self::install::{
    has_rimz_hook_command, hooks_installed_at, install_into, snake_event_token, uninstall_from,
    untrusted_hook_events_at,
};
use self::payloads::{
    CodexChildIdentity, CodexCommon, CodexPermissionRequest, CodexPostCompact, CodexPostToolUse,
    CodexPreCompact, CodexPreToolUse, CodexSessionStart, CodexStop, CodexSubagentStart,
    CodexSubagentStop, CodexUserPromptSubmit, parse_permission_request, parse_post_compact,
    parse_post_tool_use, parse_pre_compact, parse_pre_tool_use, parse_session_start, parse_stop,
    parse_subagent_start, parse_subagent_stop, parse_user_prompt_submit,
};
pub use self::process::{codex_daemon_pids, codex_resumed_session_id_from_cmdline};
use self::rollout::{CodexRolloutHeader, parse_messages, read_rollout_header};
pub(crate) use self::transcript::infer_turn_death_from_spent_window;
#[cfg(test)]
pub(crate) use self::transcript::with_codex_sessions_root;
use self::transcript::{
    RestingTurnOutcome, TranscriptScanNeed, TranscriptUsage, configured_model,
    configured_reasoning_effort, find_session_transcript, payload_reasoning_effort,
    scan_transcript_tail,
};
#[cfg(test)]
use self::transcript::{
    configured_model_at, configured_reasoning_effort_at, death_warning_from_frame,
    detect_plan_proposed, detect_turn_complete, detect_turn_error, detect_turn_interrupted,
    find_session_transcript_under, transcript_enrichment, usage_from_transcript,
    with_codex_config_path,
};
pub use self::transcript::{
    refine_turn_death_from_frame, refresh_transcript_context, session_origin,
    turn_death_needs_pane_confirmation,
};
use super::AskKind;
#[cfg(test)]
use super::TranscriptRole;
use super::context::AgentContext;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::{
    AccountUsageSnapshot, AgentLifecycleObservation, AgentTurnError, AnswerPlanErr, AnswerStep,
    AskReply, ExtraCredits, HookOutput, HookRouting, LifecycleRefreshCtx, LocalContextRefresh,
    LocalContextRefreshCtx, RefreshSpawn, RefreshTrigger, ResetCredits, Result, RootIdentity,
    SessionContextInput, SessionContextRefresh, SubagentIdentity, TranscriptMessage,
    non_empty_trimmed, optional_payload_string, read_transcript_tail, resolve_root_identity,
    resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
use crate::transcript::{AskOption, AskQuestion};

/// Per-hook timeout written into the Codex config (seconds). Hooks write a
/// Waiting state and return neutral immediately, so the value is a short guard
/// for local I/O failures rather than an answer window.
const CODEX_HOOK_TIMEOUT_SECS: i64 = 10;

/// How stale the app-server-owned half of the sidecar may get before the next
/// turn-boundary refresh re-reads it. The rollout tail refreshes every pass;
/// this throttles only the expensive app-server round trip.
const RICH_REFRESH_THROTTLE_SECS: i64 = 20;

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

/// Marker RimZ sets on every `codex app-server` it spawns for read-only
/// enrichment (the cold-spawn in [`app_server`] and the warm [`broker`]). Such a
/// server is not a user session, yet Codex still fires its configured lifecycle
/// hooks (e.g. `SessionStart`) when it starts. Those hook children inherit this
/// marker, and `rimz hooks feed` no-ops on it — which breaks the
/// `refresh-context → cold-spawn app-server → SessionStart hook →
/// context_refresh_spawn → refresh-context` recursion that would otherwise
/// spawn unboundedly. Empty value means unset.
pub const ENV_INTERNAL_APP_SERVER: &str = "RIMZ_CODEX_INTERNAL_APP_SERVER";

/// True when the current process was spawned as a RimZ-internal enrichment
/// `codex app-server` (the [`ENV_INTERNAL_APP_SERVER`] marker is present and
/// non-empty). The hook entrypoint reads this to suppress re-entrant feeds.
pub fn spawned_as_internal_app_server() -> bool {
    std::env::var_os(ENV_INTERNAL_APP_SERVER).is_some_and(|value| !value.is_empty())
}

/// Everything `const` about Codex, in one place. See [`AgentSpec`] for
/// the spec-vs-trait split.
static CODEX_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "codex",
    aliases: &[],
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
    expected_windows: &["5h", "7d"],
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
        native_ask_ui: true,
        transcript_tail_context: true,
        // Codex has no background-task parking.
        // Codex fires no `SessionStart` on a plain CLI launch — it rides the
        // first `UserPromptSubmit` — and its hooks fire from the app-server
        // with no mux pane env, so a session is unstamped. Both make a Codex
        // instance present before any session binds: the sidebar binds it to
        // its pane by cwd and renders a wired-but-unprompted `codex` pane as
        // an idle agent.
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: true,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: true,
            background_sessions: true,
        },
    },
    coverage: CODEX_COVERAGE,
    user_coverage: CODEX_USER_COVERAGE,
    lifecycle_hooks: CODEX_LIFECYCLE_HOOKS,
    default_context_window: Some(DEFAULT_CONTEXT_WINDOW),
    default_model: Some(DEFAULT_MODEL),
    // Codex commonly runs as a `node` bundle, so PID attribution accepts the
    // launcher process name beside its own.
    process_names: &["codex", "node"],
    bin_names: &["codex"],
    bin_identity: None,
    extra_bin_dirs: &[],
    // Codex logs one rollout file per session.
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("codex"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["codex", "resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["codex", "fork"],
            after_id: &[],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &[
                "--ask-for-approval",
                "never",
                "--sandbox",
                "workspace-write",
            ],
            yolo: &["--dangerously-bypass-approvals-and-sandbox"],
            plan: &[],
        },
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model", "-m"])),
            effort: Some(super::StaticPresetMatcher::ConfigKey {
                flags: &["-c", "--config"],
                key: "model_reasoning_effort",
            }),
            system_prompt_file: Some(super::StaticPresetMatcher::ConfigKey {
                flags: &["-c", "--config"],
                key: "model_instructions_file",
            }),
            append_system_prompt_file: None,
        },
    },
};

const CODEX_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "Stop + resting rollout Plan item",
    },
    user_question: ConcernCoverage::Wired {
        via: "PreToolUse:request_user_input",
    },
    answer: ConcernCoverage::Wired {
        via: "pane keystroke choreography",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact/SessionStart:compact",
    },
    subagents: ConcernCoverage::Wired {
        via: "all child-identified lifecycle hooks + child rollout enrichment",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "no SessionEnd hook; cleared on a snapshot tick, not at session exit",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + request_user_input + stall window",
        gap: "no idle Notification hook; no idle-timeout nudge",
    },
    context_usage: ConcernCoverage::Wired {
        via: "rollout tail",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "rollout tail",
    },
    rich_context: ConcernCoverage::Wired { via: "app-server" },
    hook_install: ConcernCoverage::Wired {
        via: "~/.codex/config.toml",
    },
    account_spend: ConcernCoverage::Wired {
        via: "app-server/OAuth usage/rollouts",
    },
    tool_stats: ConcernCoverage::Partial {
        via: "hook tool names + rollout response items",
        gap: "live hooks miss unified-exec and web-search calls",
    },
    remote_control: ConcernCoverage::Wired {
        via: "pane/background",
    },
};

const CODEX_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card opens at startup, follows every turn, and clears once the session is gone",
    },
    live: CapabilityLevel::Full {
        note: "the live thread keeps context fill, the token split, and the dollar current mid-turn",
    },
    history: CapabilityLevel::Full {
        note: "active and archived threads read end to end, each turn priced for stats",
    },
    account: CapabilityLevel::Full {
        note: "plan plus both rate-limit windows with their fill, reset, and credit balance",
    },
    ask: CapabilityLevel::Full {
        note: "approvals, plans, and questions raise Waiting and reach rimz asks with their options",
    },
    subagents: CapabilityLevel::Full {
        note: "child threads nest under the parent as they start, with name, role, model, and tokens",
    },
};

const CODEX_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "no SessionEnd hook; cleared on a snapshot tick, not at session exit",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

/// Installed events and classification policy — the single source of truth for
/// which Codex events RimZ wires and with which matcher, mirroring the Claude
/// adapter's catalog. `SessionStart` filters to its
/// lifecycle subtypes; the per-call hooks match everything (`.*`); the
/// turn-boundary events (`UserPromptSubmit`, `Stop`) carry no matcher.
/// `UserPromptSubmit` is state signal — it moves the root agent to running and
/// carries the task. The broad `PreToolUse`/`PostToolUse` hooks fire on every
/// tool call; they keep the sidebar's enrichment current, with their payload
/// content gated by `[privacy] payload_mode`.
const CODEX_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "SessionStart",
        r#"{"session_id":"sess-1","source":"startup"}"#,
    )
    .with_matcher("startup|resume|clear|compact")
    .progress(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"session_id":"sess-1","prompt":"fix auth"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"session_id":"sess-parent","agent_id":"child-thread-1","agent_type":"review"}"#,
    )
    .with_matcher(".*")
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"session_id":"sess-parent","agent_id":"child-thread-1","agent_type":"review"}"#,
    )
    .with_matcher(".*")
    .progress(),
    HookEventSpec::lifecycle("Stop", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::blocking(
        "PermissionRequest",
        r#"{"session_id":"sess-1","tool_name":"shell"}"#,
        AskKind::Permission,
    )
    .with_matcher(".*")
    .synchronous(),
    HookEventSpec::lifecycle(
        "PreToolUse",
        r#"{"session_id":"sess-1","tool_name":"shell"}"#,
    )
    .with_matcher(".*"),
    HookEventSpec::lifecycle(
        "PostToolUse",
        r#"{"session_id":"sess-1","tool_name":"apply_patch"}"#,
    )
    .with_matcher(".*")
    .progress(),
    HookEventSpec::lifecycle(
        "PreCompact",
        r#"{"session_id":"sess-1","trigger":"manual"}"#,
    )
    .with_matcher(".*"),
    HookEventSpec::lifecycle(
        "PostCompact",
        r#"{"session_id":"sess-1","trigger":"manual"}"#,
    )
    .with_matcher(".*"),
];

/// Legacy config block written by older RimZ builds. Codex ignores this block;
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

fn hook_ingress_decision(
    pid: Option<u32>,
    internal_app_server: bool,
    daemon_owned: bool,
) -> super::HookIngressDecision {
    if internal_app_server {
        return super::HookIngressDecision::Ignore(
            super::HookIngressIgnoreReason::CodexInternalAppServer,
        );
    }
    let kind = if daemon_owned {
        crate::pane::RuntimeOwnerKind::Daemon
    } else {
        crate::pane::RuntimeOwnerKind::Agent
    };
    super::HookIngressDecision::Accept(super::HookIngressAcceptance {
        owner: super::HookIngressOwner { pid, kind },
        participant_start: None,
    })
}

impl crate::agents::capabilities::CoreCapability for CodexAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &CODEX_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        use super::{AgentHookClass, ClassificationSample};

        let mut samples = super::hook_types::catalog_classification_corpus(CODEX_HOOKS);
        samples.extend([ClassificationSample::new(
            "PreToolUse",
            serde_json::json!({ "session_id": "sess-1", "tool_name": "request_user_input" }),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Question),
        )]);
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "sess-1",
                file_name: "rollout-2026-06-02T10-00-00-sess-1.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"timestamp":"2026-06-02T10:00:00.000Z","model":"gpt-5","usage":{"input_tokens":100,"output_tokens":50}}"#,
                ),
            }),
            derived_ask: Some(super::DerivedAskFixture {
                event_name: "Stop",
                payload: serde_json::json!({
                    "session_id": "sess-plan",
                    "turn_id": "turn-plan",
                    "last_assistant_message": "Codex says:"
                }),
                transcript_file_name: "rollout-plan.jsonl",
                transcript_body: concat!(
                    r##"{"timestamp":"2026-07-13T10:00:00Z","type":"turn_context","payload":{"turn_id":"turn-plan","collaboration_mode":{"mode":"plan"}}}"##,
                    "\n",
                    r##"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","id":"turn-plan-plan","text":"# Plan\n\nShip it."}}}"##,
                    "\n",
                    r##"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"Codex says:"}}"##,
                    "\n",
                    r##"{"timestamp":"2026-07-13T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","last_agent_message":"Codex says:"}}"##,
                ),
                expected_kind: AskKind::PlanApproval,
            }),
            local_session: Some(local_sessions::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for CodexAdapter {
    fn hook_ingress(&self, pid: Option<u32>) -> super::HookIngressDecision {
        hook_ingress_decision(
            pid,
            spawned_as_internal_app_server(),
            pid.is_some_and(process::pid_is_codex_daemon),
        )
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let parts = CodexLifecycleParts::parse(event_name, payload);
        let ask_kind = match event_name {
            "PermissionRequest" => Some(AskKind::Permission),
            "PreToolUse" => self.spec().blocking_tool_kind(
                parts
                    .pre_tool_use
                    .as_ref()
                    .and_then(|request| request.tool_name.as_deref()),
            ),
            _ => None,
        };
        let mut decoded = decode_catalog_hook(CODEX_HOOKS, event_name, ask_kind);
        decoded.set_routing(
            HookRouting::split(
                optional_payload_string(payload, &["agent_id", "session_id"]).map(Into::into),
                optional_payload_string(payload, &["session_id", "agent_id"]).map(Into::into),
            )
            .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"]))
            .with_server_url(optional_payload_string(payload, &["server_url"])),
        );
        decoded.set_native_answers(match event_name {
            "PostToolUse" => parts.post_tool_use.as_ref().and_then(|parsed| {
                ask::answer_detail(
                    parsed.tool_name.as_deref()?,
                    parsed.tool_input.as_ref()?,
                    parsed.tool_response.as_ref()?,
                )
            }),
            "UserPromptSubmit" => parts
                .user_prompt
                .as_ref()
                .and_then(|parsed| ask::submitted_prompt_answer(parsed.prompt.as_deref()?)),
            _ => None,
        });
        let child_id = parts.distinct_child_id();
        let transcript = codex_transcript_observation(
            payload,
            child_id,
            matches!(event_name, "Stop" | "SubagentStop"),
        );
        let questions = match event_name {
            "PreToolUse" => parts
                .pre_tool_use
                .as_ref()
                .and_then(|parsed| {
                    ask::question_detail(parsed.tool_name.as_deref()?, parsed.tool_input.as_ref()?)
                })
                .unwrap_or_default(),
            "Stop" => transcript
                .plan_proposed
                .as_ref()
                .and_then(|plan| ask::plan_question(&plan.text))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let ask_detail = questions
            .first()
            .and_then(|question| question.question.lines().next())
            .map(ToOwned::to_owned)
            .filter(|detail| !detail.is_empty());
        decoded.set_ask(questions, ask_detail);
        decoded.set_turn_error(transcript.turn_error.clone());
        decoded.set_final_message(
            (event_name == "Stop")
                .then_some(parts.stop.as_ref())
                .flatten()
                .and_then(|stop| stop.last_assistant_message.as_deref())
                .and_then(non_empty_trimmed),
        );
        let signal = map_codex_lifecycle_signal(
            self.spec(),
            event_name,
            payload,
            &parts,
            transcript.turn_error.as_ref(),
            transcript.plan_proposed.is_some(),
        );
        if let Some(signal) = signal
            && let Some((agent_id, parent_agent_id)) =
                resolve_codex_observation_identity(self.spec().kind, event_name, payload, &parts)
        {
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
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }

    fn ask_options(&self, kind: AskKind) -> Option<Vec<AskOption>> {
        match kind {
            AskKind::PlanApproval => Some(ask::plan_options()),
            AskKind::Permission | AskKind::Question => None,
        }
    }

    fn answer_plan(
        &self,
        kind: AskKind,
        questions: &[AskQuestion],
        answers: &[AskReply],
    ) -> std::result::Result<Vec<AnswerStep>, AnswerPlanErr> {
        ask::answer_plan(kind, questions, answers)
    }
}

impl crate::agents::capabilities::InstallationCapability for CodexAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

impl crate::agents::capabilities::LaunchCapability for CodexAdapter {
    fn is_interactive_process(&self, command: &str) -> bool {
        process::is_interactive_process(command)
    }

    fn default_launch_model(&self) -> Option<String> {
        configured_model().or_else(|| self.spec().default_model.map(ToOwned::to_owned))
    }

    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (configured_model(), configured_reasoning_effort())
    }
}

impl crate::agents::capabilities::SessionCapability for CodexAdapter {
    fn daemon_session_evidence(&self) -> super::session::DaemonSessionEvidence {
        let pids = codex_daemon_pids();
        let loaded_session_ids = (!pids.is_empty()).then(loaded_daemon_threads).flatten();
        super::session::DaemonSessionEvidence {
            pids,
            loaded_session_ids,
        }
    }

    fn turn_death_needs_pane_confirmation(&self, error: &AgentTurnError) -> bool {
        turn_death_needs_pane_confirmation(error)
    }

    fn refine_turn_death_from_frame(&self, error: &mut AgentTurnError, frame: &str) {
        refine_turn_death_from_frame(error, frame);
    }

    fn infer_turn_death_from_spent_window(
        &self,
        error: &mut AgentTurnError,
        capacity: Option<&super::ProviderCapacity>,
        now: Timestamp,
    ) {
        infer_turn_death_from_spent_window(error, capacity, now);
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<super::LocalSessionObservation> {
        local_sessions::discover(workspaces)
    }

    fn probe_resting_interruption(
        &self,
        agent_id: &crate::ids::AgentSessionId,
    ) -> Option<Timestamp> {
        transcript::resting_interruption(agent_id.as_str())
    }

    /// `codex resume <id>` resolves the UUID to its rollout file and restores
    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        codex_resumed_session_id_from_cmdline(cmdline)
    }
}

impl crate::agents::capabilities::TranscriptCapability for CodexAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        parse_messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for CodexAdapter {
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
        let mut args = crate::agents::refresh_context_argv(self.spec().kind, ctx);
        if let Some(model) = ctx.model_hint {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        Some(RefreshSpawn { args })
    }

    /// Two sources in one pass: the local rollout tail always, and the
    /// app-server's read-only enrichment (rate-limit windows, model display
    /// name, thread name/preview, version) only when its own fields are stale.
    /// The app-server read is the expensive half, so its throttle lives here
    /// rather than at the call site.
    fn refresh_session_context(
        &self,
        input: &SessionContextInput<'_>,
    ) -> Option<SessionContextRefresh> {
        let model_hint = input.model.or_else(|| {
            input
                .prior
                .and_then(|record| record.context.model_id.as_deref())
        });
        let local = refresh_transcript_context(
            input.session_id,
            model_hint,
            input
                .prior
                .and_then(|record| record.transcript_path.as_deref()),
            input
                .prior
                .and_then(|record| record.transcript_stat.as_ref()),
            input.prior.and_then(|record| record.spend_fold.as_ref()),
            input.pricing_cache_path,
        );
        if !app_server_due(input.prior, RICH_REFRESH_THROTTLE_SECS) {
            return local.map(|local| SessionContextRefresh {
                local: Some(local),
                ..SessionContextRefresh::default()
            });
        }
        let enrichment =
            refresh_app_server_enrichment(Some(input.session_id), input.model, input.broker_socket);
        let realtime_usage = enrichment
            .as_ref()
            .map(|enrichment| crate::AccountUsageSnapshot {
                plan: enrichment
                    .context
                    .account
                    .as_ref()
                    .and_then(|account| account.plan.clone()),
                rate_limits: enrichment.context.rate_limits.clone(),
                extra_credits: enrichment.extra_credits.clone(),
                reset_credits: enrichment.reset_credits.clone(),
            });
        Some(SessionContextRefresh {
            local,
            observed: enrichment.map(|enrichment| enrichment.context),
            realtime_usage,
        })
    }

    fn merge_session_context(
        &self,
        record: &mut crate::store::agent_context::AgentContextRecord,
        observed: &AgentContext,
    ) -> bool {
        merge_app_server_context(record, observed)
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
            ctx.prior_spend_fold,
            ctx.shared_pricing_cache_path,
        )
    }
}

impl crate::agents::capabilities::AccountCapability for CodexAdapter {
    fn prepare_reset_credit(
        &self,
    ) -> std::result::Result<super::account::ResetCreditOffer, String> {
        let (credentials, base_url) =
            oauth_usage::load_configured_credentials().map_err(|error| error.to_string())?;
        let identity = credentials.account_usage_identity();
        let usage = oauth_usage::fetch_usage_with_url(
            &oauth_usage::usage_url(base_url.as_deref()),
            &credentials,
        )
        .map_err(|error| error.to_string())?;
        let (credits, details) = oauth_usage::fetch_reset_credit_state(
            &oauth_usage::reset_credits_url(base_url.as_deref()),
            &credentials,
        )
        .map_err(|error| error.to_string())?;
        let capacity = usage
            .rate_limits
            .as_ref()
            .map(|limits| super::ProviderCapacity::from_windows(limits.windows.clone()));
        let credit_id = oauth_usage::select_reset_credit_id(&details).map(ToOwned::to_owned);
        Ok(super::account::ResetCreditOffer::new(
            capacity,
            credits,
            CodexResetCreditAction {
                credentials,
                base_url,
                credit_id,
                identity,
            },
        ))
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        oauth_usage::probe_usage()
    }

    fn probe_realtime_account_usage(
        &self,
        runtime: &crate::RuntimePaths,
    ) -> Option<AccountUsageSnapshot> {
        refresh_app_server_enrichment(None, None, Some(&runtime.codex_app_server_socket_path()))
            .map(|enrichment| {
                let plan = enrichment
                    .context
                    .account
                    .as_ref()
                    .and_then(|account| account.plan.clone());
                AccountUsageSnapshot {
                    plan,
                    rate_limits: enrichment.context.rate_limits,
                    extra_credits: enrichment.extra_credits,
                    reset_credits: enrichment.reset_credits,
                }
            })
    }
}

struct CodexResetCreditAction {
    credentials: oauth_usage::CodexOauthCredentials,
    base_url: Option<String>,
    credit_id: Option<String>,
    identity: super::AccountUsageIdentity,
}

impl super::account::ResetCreditAction for CodexResetCreditAction {
    fn consume(
        self: Box<Self>,
        request_id: &str,
    ) -> std::result::Result<super::account::ResetCreditResult, String> {
        let outcome = oauth_usage::consume_reset_credit(
            &self.credentials,
            self.base_url.as_deref(),
            request_id,
            self.credit_id.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let code = match outcome.code {
            oauth_usage::ConsumeCode::Reset => super::account::RedemptionCode::Reset,
            oauth_usage::ConsumeCode::NothingToReset => {
                super::account::RedemptionCode::NothingToReset
            }
            oauth_usage::ConsumeCode::NoCredit => super::account::RedemptionCode::NoCredit,
            oauth_usage::ConsumeCode::AlreadyRedeemed => {
                super::account::RedemptionCode::AlreadyRedeemed
            }
            oauth_usage::ConsumeCode::Unknown => super::account::RedemptionCode::Unknown,
        };
        let (refreshed, refresh_error) = if code == super::account::RedemptionCode::Reset {
            match oauth_usage::fetch_usage_with_url(
                &oauth_usage::usage_url(self.base_url.as_deref()),
                &self.credentials,
            ) {
                Ok(mut snapshot) => {
                    snapshot.reset_credits = oauth_usage::fetch_reset_credit_state(
                        &oauth_usage::reset_credits_url(self.base_url.as_deref()),
                        &self.credentials,
                    )
                    .ok()
                    .map(|(credits, _)| credits);
                    (Some((self.identity, snapshot)), None)
                }
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };
        Ok(super::account::ResetCreditResult {
            outcome: code,
            windows_reset: outcome.windows_reset,
            refreshed,
            refresh_error,
        })
    }
}

impl crate::agents::capabilities::SpendingCapability for CodexAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        spend::codex_homes()
            .into_iter()
            .filter_map(|home| {
                let active = crate::agents::spending::SpendingSourceTree::new(
                    home.join("sessions"),
                    "**/*.jsonl",
                )?
                .codex_dates();
                let archived = crate::agents::spending::SpendingSourceTree::new(
                    home.join("archived_sessions"),
                    "**/*.jsonl",
                )?
                .codex_dates();
                let legacy = crate::agents::spending::SpendingSourceTree::new(home, "**/*.jsonl")?
                    .filtered("codex-legacy", spend::legacy_spend_relative)
                    .descend_filtered("codex-legacy-dirs", spend::legacy_spend_relative);
                Some(crate::agents::spending::SpendingSource::group(vec![
                    active, archived, legacy,
                ]))
            })
            .collect()
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

impl crate::agents::capabilities::RuntimeControlCapability for CodexAdapter {
    fn runtime_control_readiness(
        &self,
        enabled: bool,
    ) -> super::runtime_control::RuntimeControlReadiness {
        match app_server::daemon::readiness(enabled) {
            app_server::daemon::Readiness::Disabled => {
                super::runtime_control::RuntimeControlReadiness::Disabled
            }
            app_server::daemon::Readiness::Ready => {
                super::runtime_control::RuntimeControlReadiness::Ready { host_argv: None }
            }
            app_server::daemon::Readiness::Uninstalled(issue) => {
                super::runtime_control::RuntimeControlReadiness::Uninstalled(
                    super::runtime_control::RuntimeControlIssue::new(
                        "codex",
                        "standalone_missing",
                        &issue,
                    ),
                )
            }
        }
    }

    fn ensure_runtime_control(&self, enabled: bool) {
        app_server::daemon::ensure(enabled);
    }

    fn reconcile_runtime_control(
        &self,
        enabled: bool,
    ) -> std::result::Result<(), super::runtime_control::RuntimeControlError> {
        app_server::daemon::reconcile(enabled)
            .map_err(|error| super::runtime_control::RuntimeControlError::new("codex", error))
    }

    fn runtime_control_advisory(&self) -> Option<String> {
        app_server::daemon::updater_skew().map(|skew| skew.to_string())
    }
}

struct CodexLifecycleParts {
    session_start: Option<CodexSessionStart>,
    user_prompt: Option<CodexUserPromptSubmit>,
    subagent_start: Option<CodexSubagentStart>,
    subagent_stop: Option<CodexSubagentStop>,
    pre_tool_use: Option<CodexPreToolUse>,
    permission_request: Option<CodexPermissionRequest>,
    post_tool_use: Option<CodexPostToolUse>,
    pre_compact: Option<CodexPreCompact>,
    post_compact: Option<CodexPostCompact>,
    stop: Option<CodexStop>,
}

struct CodexChild<'a> {
    identity: &'a CodexChildIdentity,
    common: &'a CodexCommon,
}

impl CodexChild<'_> {
    fn is_distinct(&self) -> bool {
        let child = self
            .identity
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let parent = self
            .common
            .common
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        matches!((child, parent), (Some(child), Some(parent)) if child != parent)
    }
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
            post_tool_use: (event_name == "PostToolUse").then(|| parse_post_tool_use(payload)),
            pre_compact: (event_name == "PreCompact").then(|| parse_pre_compact(payload)),
            post_compact: (event_name == "PostCompact").then(|| parse_post_compact(payload)),
            stop: (event_name == "Stop").then(|| parse_stop(payload)),
        }
    }

    fn child(&self) -> Option<CodexChild<'_>> {
        self.subagent_start
            .as_ref()
            .map(|p| CodexChild {
                identity: &p.child,
                common: &p.common,
            })
            .or_else(|| {
                self.subagent_stop.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.user_prompt.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.pre_tool_use.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.permission_request.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.post_tool_use.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.pre_compact.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
            .or_else(|| {
                self.post_compact.as_ref().map(|p| CodexChild {
                    identity: &p.child,
                    common: &p.common,
                })
            })
    }

    fn distinct_child_id(&self) -> Option<&str> {
        let child = self.child()?;
        child
            .is_distinct()
            .then_some(child.identity.agent_id.as_deref())
            .flatten()
    }

    fn hook_model(&self) -> Option<String> {
        self.session_start
            .as_ref()
            .and_then(|session| session.common.model.clone())
            .or_else(|| self.child().and_then(|child| child.common.model.clone()))
    }
}

fn map_codex_lifecycle_signal(
    spec: &AgentSpec,
    event_name: &str,
    payload: &Value,
    parts: &CodexLifecycleParts,
    turn_error: Option<&AgentTurnError>,
    plan_proposed: bool,
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
        "SubagentStop" => Some(LifecycleSignal::SubagentStopped {
            errored: stop_payload_errored(payload) || turn_error.is_some(),
        }),
        "Stop" if plan_proposed => Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ask_id: None,
            detail: None,
            native_key: None,
        }),
        "Stop" => Some(LifecycleSignal::TurnEnded {
            errored: stop_payload_errored(payload) || turn_error.is_some(),
            parked_on_background: false,
        }),
        "PermissionRequest" => Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: None,
        }),
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: spec.tool_mutates(payload),
            edits: spec.tool_edits_files(payload),
            name: parts
                .post_tool_use
                .as_ref()
                .and_then(|tool| tool.tool_name.clone()),
            native_key: None,
        }),
        "PreToolUse" => {
            match spec.blocking_tool_kind(
                parts
                    .pre_tool_use
                    .as_ref()
                    .and_then(|p| p.tool_name.as_deref()),
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
                    name: None,
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
        _ => None,
    }
}

fn codex_transcript_path(payload: &Value) -> Option<PathBuf> {
    optional_payload_string(payload, &["transcript_path"])
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            optional_payload_string(payload, &["session_id"])
                .and_then(|id| find_session_transcript(&id))
        })
}

struct CodexChildTranscript {
    path: PathBuf,
    validated_header: Option<CodexRolloutHeader>,
}

fn codex_child_transcript_path(payload: &Value, child_id: &str) -> Option<CodexChildTranscript> {
    optional_payload_string(payload, &["agent_transcript_path"])
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .map(|path| CodexChildTranscript {
            path,
            validated_header: None,
        })
        .or_else(|| {
            let path = optional_payload_string(payload, &["transcript_path"])
                .map(PathBuf::from)
                .filter(|path| path.is_file())?;
            let header = read_rollout_header(&path)?;
            (header.session_id.as_deref() == Some(child_id)).then_some(CodexChildTranscript {
                path,
                validated_header: Some(header),
            })
        })
        .or_else(|| {
            find_session_transcript(child_id).map(|path| CodexChildTranscript {
                path,
                validated_header: None,
            })
        })
}

struct CodexTranscriptObservation {
    path: Option<PathBuf>,
    header: Option<CodexRolloutHeader>,
    usage: TranscriptUsage,
    turn_error: Option<AgentTurnError>,
    plan_proposed: Option<self::transcript::PlanProposal>,
}

fn codex_transcript_observation(
    payload: &Value,
    child_id: Option<&str>,
    detect_turn_death: bool,
) -> CodexTranscriptObservation {
    let (path, validated_header) = match child_id {
        Some(child_id) => codex_child_transcript_path(payload, child_id)
            .map(|transcript| (Some(transcript.path), transcript.validated_header))
            .unwrap_or_default(),
        None => (codex_transcript_path(payload), None),
    };
    let header = child_id.and_then(|child_id| {
        let header = validated_header.or_else(|| path.as_deref().and_then(read_rollout_header))?;
        (header.session_id.as_deref() == Some(child_id)).then_some(header)
    });
    let tail = path.as_deref().and_then(read_transcript_tail);
    let need = if detect_turn_death {
        TranscriptScanNeed::UsageAndOutcome
    } else {
        TranscriptScanNeed::UsageOnly
    };
    let (usage, outcome, turn_error) = tail
        .as_deref()
        .map(|tail| scan_transcript_tail(tail, need).into_parts())
        .unwrap_or_default();
    let plan_proposed = match outcome {
        Some(RestingTurnOutcome::PlanProposed(plan)) => Some(plan),
        Some(
            RestingTurnOutcome::Complete(_)
            | RestingTurnOutcome::Interrupted(_)
            | RestingTurnOutcome::Died(_),
        )
        | None => None,
    };
    CodexTranscriptObservation {
        path,
        header,
        usage,
        turn_error,
        plan_proposed,
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
) -> Option<ObservationIdentity> {
    let child = parts.child();
    let subagent_event = matches!(event_name, "SubagentStart" | "SubagentStop")
        || child.as_ref().is_some_and(CodexChild::is_distinct);
    if subagent_event {
        let child_id = child
            .as_ref()
            .and_then(|child| child.identity.agent_id.as_deref());
        let parent_id = child
            .as_ref()
            .and_then(|child| child.common.common.session_id.as_deref());
        match resolve_subagent_identity(kind, event_name, child_id, parent_id, payload) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => Some((Some(agent_id), Some(parent_agent_id))),
            SubagentIdentity::Quarantined => None,
        }
    } else {
        let typed_agent_id = child
            .as_ref()
            .and_then(|child| child.identity.agent_id.as_deref());
        let typed_session_id = child
            .as_ref()
            .and_then(|child| child.common.common.session_id.as_deref());
        let payload_agent_id = optional_payload_string(payload, &["agent_id"]);
        let payload_session_id = optional_payload_string(payload, &["session_id"]);
        match resolve_root_identity(
            kind,
            event_name,
            typed_agent_id.or(payload_agent_id.as_deref()),
            typed_session_id.or(payload_session_id.as_deref()),
        ) {
            RootIdentity::Root { agent_id } => Some((agent_id, None)),
            RootIdentity::ForeignChild => None,
        }
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
    let header = transcript
        .header
        .as_ref()
        .filter(|header| header.is_subagent);
    let usage = transcript.usage;
    let usage_effort = usage.effort.clone();
    let is_subagent = parent_agent_id.is_some();
    let agent_type = is_subagent
        .then(|| parts.child()?.identity.agent_type.clone())
        .flatten();
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    observation.parent_agent_id = parent_agent_id;
    observation.agent_name = is_subagent
        .then(|| {
            header
                .and_then(|header| header.agent_nickname.clone())
                .or_else(|| agent_type.clone())
        })
        .flatten();
    observation.task = if is_subagent {
        header
            .and_then(|header| header.agent_path.clone())
            .or_else(|| agent_type.clone())
    } else {
        sanitize_user_prompt(optional_payload_string(payload, &["task", "prompt"]).as_deref())
    };
    observation.prompt =
        sanitize_user_prompt(parts.user_prompt.as_ref().and_then(|p| p.prompt.as_deref()));
    observation.transcript_path = transcript
        .path
        .map(|path| path.to_string_lossy().into_owned());
    let reported_context_window = usage.reported_context_window();
    observation.launch.role = is_subagent
        .then(|| {
            header
                .and_then(|header| header.agent_role.clone())
                .or_else(|| agent_type.clone())
        })
        .flatten();
    observation.launch.model = parts
        .hook_model()
        .or_else(|| optional_payload_string(payload, &["model"]))
        .or(usage.model)
        .or_else(|| is_subagent.then(configured_model).flatten());
    observation.launch.effort = payload_reasoning_effort(payload)
        .or(usage_effort)
        .or_else(|| is_subagent.then(configured_reasoning_effort).flatten());
    observation.usage.context_window = reported_context_window;
    observation.usage.total_tokens = if is_subagent {
        usage.total_tokens
    } else {
        payload_total_tokens(payload, usage.total_tokens)
    };
    observation.usage.cache_read_input_tokens = usage.last_cached_input_tokens;
    observation.usage.cache_write_input_tokens = usage.last_cache_write_tokens;
    observation.usage.fresh_input_tokens = usage.last_input_tokens.map(|input| {
        input
            .saturating_sub(usage.last_cached_input_tokens.unwrap_or(0))
            .saturating_sub(usage.last_cache_write_tokens.unwrap_or(0))
    });
    observation.usage.output_tokens = usage.last_output_tokens;
    observation
}

/// Read Codex's read-only realtime details from the app-server and project them
/// onto an [`AgentContext`] for the session sidecar. Spawned out-of-band by
/// `rimz agents refresh-context` (never inline in a hook). The app-server owns
/// rate-limit windows, account plan, model display name, thread preview/name,
/// and version.
/// Transcript-derived tokens and cost are refreshed separately from the local
/// rollout tail, so an unreachable app-server never suppresses them.
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

#[cfg(test)]
mod tests;
