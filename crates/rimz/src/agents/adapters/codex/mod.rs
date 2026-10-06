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
//! `$CODEX_HOME/config.toml` (`~/.codex` by default) using Codex's inline `[[hooks.Event]]` tables.
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
//! The local session index supplies Codex's automatic thread name inline.
//! Remaining metadata Claude gets from its statusline (rate-limit windows,
//! model display name, thread preview, version) comes from the app-server
//! read-only methods, throttled by [`app_server_due`] and spawned out-of-band
//! by `rimz agents refresh-context`.

mod account;
mod app_server;
mod ask;
pub(in crate::agents) mod broker;
mod install;
mod local_sessions;
mod model_alias;
pub(in crate::agents) mod oauth_usage;
mod payloads;
mod process;
mod project_trust;
mod rollout;
mod session_index;
mod spend;
mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use jiff::Timestamp;

use self::app_server::{AppServerObservation, CodexAppServer};
use self::app_server::{app_server_due, merge_app_server_context};
use self::payloads::{
    CodexChildIdentity, CodexCommon, CodexPermissionRequest, CodexPostCompact, CodexPostToolUse,
    CodexPreCompact, CodexPreToolUse, CodexSessionStart, CodexStop, CodexSubagentStart,
    CodexSubagentStop, CodexUserPromptSubmit,
};
use self::process::{codex_daemon_pids, codex_resumed_session_id_from_cmdline};
use self::rollout::{CodexRolloutHeader, parse_messages, read_rollout_header};
use self::transcript::infer_turn_death_from_spent_window;
use self::transcript::{
    RestingTurnOutcome, TranscriptScanNeed, TranscriptUsage, configured_model,
    configured_reasoning_effort, find_session_transcript, payload_reasoning_effort,
    scan_transcript_tail, session_forked_from,
};
use self::transcript::{
    refine_turn_death_from_frame, refresh_transcript_context, session_origin,
    turn_death_needs_pane_confirmation,
};
use super::AskKind;
use super::context::AgentContext;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookContextReply, HookCoverage, LifecycleAnnotations, PlanLabel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::observation::{SessionOrigin, payload_total_tokens};
use super::pricing::PriceBook;
use super::{
    AccountUsageSnapshot, AgentLifecycleObservation, AgentTurnError, AnswerPlanErr, AnswerStep,
    AskReply, FieldPatch, HookOutput, HookRouting, LifecycleRefreshCtx, LocalContextRefresh,
    LocalContextRefreshCtx, RefreshSpawn, RefreshTrigger, Result, RootIdentity, SanitizedPrompt,
    SessionContextInput, SessionContextRefresh, SubagentIdentity, TranscriptMessage,
    non_empty_trimmed, optional_payload_string, read_transcript_tail, resolve_root_identity,
    resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
use crate::transcript::{AskOption, AskQuestion};

/// Per-hook timeout written into the Codex config (seconds). Hooks write a
/// Waiting state and return neutral immediately, so the value is a short guard
/// for local I/O failures rather than an answer window.
const CODEX_HOOK_TIMEOUT_SECS: i64 = 10;
/// Codex awaits `Interrupt` inline and defaults it to one second (with a
/// three-second ceiling), so keep Ctrl-C responsive and let an overrun fall
/// back to the flushed rollout's `turn_aborted` record.
const CODEX_INTERRUPT_HOOK_TIMEOUT_SECS: i64 = 1;

/// Codex's GPT-5.5 backend input ceiling — the observed 272k-token limit above
/// which the Codex backend rejects a prompt, listed by litellm and models.dev
/// as the Codex-family `max_input_tokens` / `limit.input`. The rollout's
/// `model_context_window` — Codex's effective window after its internal headroom
/// (`258_400 = 272k × 95%`) — replaces this as soon as it appears; until then the
/// agent card uses this stable provider fallback instead of briefly omitting the
/// window token.
const DEFAULT_CONTEXT_WINDOW: u64 = 272_000;

/// Marker RimZ sets on every `codex app-server` it spawns for read-only
/// enrichment (the cold-spawn in [`app_server`] and the warm [`broker`]). Such a
/// server is not a user session, yet Codex still fires its configured lifecycle
/// hooks (e.g. `SessionStart`) when it starts. Those hook children inherit this
/// marker, and `rimz hooks feed` no-ops on it — which breaks the
/// `refresh-context → cold-spawn app-server → SessionStart hook →
/// context_refresh_spawn → refresh-context` recursion that would otherwise
/// spawn unboundedly. Empty value means unset.
const ENV_INTERNAL_APP_SERVER: &str = "RIMZ_CODEX_INTERNAL_APP_SERVER";

/// True when the current process was spawned as a RimZ-internal enrichment
/// `codex app-server` (the [`ENV_INTERNAL_APP_SERVER`] marker is present and
/// non-empty). The hook entrypoint reads this to suppress re-entrant feeds.
fn spawned_as_internal_app_server() -> bool {
    std::env::var_os(ENV_INTERNAL_APP_SERVER).is_some_and(|value| !value.is_empty())
}

/// Everything `const` about Codex, in one place. See [`AgentSpec`] for
/// the spec-vs-trait split.
static CODEX_DESCRIPTOR: AgentSpec = AgentSpec {
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Switch {
        flag: "-c skills.config",
        effect: "hidden",
        key: host_skill_key,
        render: render_host_skills,
    },
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
        input_key: Some("tool_input"),
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
        // `ExitPlanMode` calls.
        blocking: &[("request_user_input", AskKind::Question)],
    },
    capabilities: Capabilities {
        hook_context: Some(HookContextReply::HookSpecificOutput { event_name: true }),
        prompt_context: true,
        native_ask_ui: true,
        transcript_tail_context: true,
        // Codex has no background-task parking.
        // Codex fires no `SessionStart` on a plain CLI launch — it rides the
        // first `UserPromptSubmit`. Managed panes run embedded (--no-daemon);
        // a user-run `codex` may still be daemon-routed and arrive unstamped.
        // The sidebar binds an instance to its pane by cwd before a session
        // binds and renders a wired-but-unprompted pane as an idle agent.
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: true,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: true,
        },
    },
    coverage: CODEX_COVERAGE,
    user_coverage: CODEX_USER_COVERAGE,
    lifecycle_hooks: CODEX_LIFECYCLE_HOOKS,
    default_context_window: Some(DEFAULT_CONTEXT_WINDOW),
    // Codex owns its native default; do not pin launches or guess idle identity.
    default_model: None,
    // Codex commonly runs as a `node` bundle, so PID attribution accepts the
    // launcher process name beside its own.
    process_names: &["codex", "node"],
    bin_names: &["codex"],
    bin_identity: None,
    extra_bin_dirs: &[],
    // Codex logs one rollout file per session.
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        definitions: DEFINITIONS,
        program: Some("codex"),
        fixed_args: &["--no-daemon"],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["codex", "resume"],
            after_id: &["--no-daemon"],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["codex", "fork"],
            after_id: &["--no-daemon"],
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
        interrupt_key: Some(crate::pane::keys::NamedKey::Escape),
        compact_command: Some(super::CompactCommand {
            command: "/compact",
            instruction: super::CompactInstruction::Unsupported,
        }),
        presets: super::PresetMatchers {
            auto_compact: Some(super::StaticPresetMatcher::ConfigKey {
                flags: &["-c", "--config"],
                key: "model_auto_compact_token_limit",
            }),
            model: Some(super::StaticPresetMatcher::Flag(&["--model", "-m"])),
            effort: Some(super::StaticPresetMatcher::ConfigKey {
                flags: &["-c", "--config"],
                key: "model_reasoning_effort",
            }),
            system_prompt_file: Some(super::StaticPresetMatcher::ConfigKey {
                flags: &["-c", "--config"],
                key: "model_instructions_file",
            }),
        },
    },
};

const CODEX_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop/Interrupt",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "Stop + resting rollout Plan item",
    },
    user_question: ConcernCoverage::Wired {
        via: "PreToolUse:request_user_input + PostToolUse:request_user_input_async",
    },
    answer: ConcernCoverage::Wired {
        via: "blocking pane keystrokes; async questions surface for answers in the pane",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact/SessionStart:compact",
    },
    subagents: ConcernCoverage::Wired {
        via: "all child-identified lifecycle hooks + child rollout enrichment",
    },
    launch_reminders: ConcernCoverage::Wired {
        via: "-c developer_instructions",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    background_shells: ConcernCoverage::Unsupported {
        reason: "shells run through code_mode exec/wait tools that fire no PreToolUse/PostToolUse, and Stop carries no task list",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "SessionEnd hook left unwired (fires on idle unload); cleared on a snapshot tick, not at session exit",
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
    rich_context: ConcernCoverage::Wired {
        via: "session index + app-server",
    },
    hook_install: ConcernCoverage::Wired {
        via: "$CODEX_HOME/config.toml (~/.codex by default)",
    },
    account_spend: ConcernCoverage::Wired {
        via: "app-server/OAuth usage/rollouts",
    },
    tool_stats: ConcernCoverage::Partial {
        via: "hook tool names + rollout response items",
        gap: "live hooks miss web-search and other non-hooked calls",
    },
    remote_control: ConcernCoverage::Wired { via: "background" },
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
        note: "blocking prompts surface; async questions surface and are answered in the pane",
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
        gap: "SessionEnd hook left unwired (fires on idle unload); cleared on a snapshot tick, not at session exit",
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
/// turn-boundary events (`UserPromptSubmit`, `Stop`, `Interrupt`) carry no matcher.
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
    HookEventSpec::lifecycle("Interrupt", r#"{"session_id":"sess-1","turn_id":"turn-1"}"#)
        .progress()
        .with_timeout(CODEX_INTERRUPT_HOOK_TIMEOUT_SECS)
        .optional_for_preflight(),
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
pub(in crate::agents) struct CodexAdapter;

const DEFINITIONS: crate::agents::definition::DefinitionSpec =
    crate::agents::definition::DefinitionSpec {
        mode: None,
        effort: Some("xhigh"),
        models: &[
            crate::agents::definition::DefinitionModel {
                name: "astra",
                id: "gpt-6-astra",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "sol",
                id: "gpt-6-sol",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "luna",
                id: "gpt-6-luna",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "terra",
                id: "gpt-5.6-terra",
                effort: None,
            },
        ],
        prefixes: &["gpt-"],
        tools: crate::agents::definition::DefinitionTools::Required(render_definition_tools),
    };

fn host_skill_key(
    skill: &crate::agents::skills::SkillDir,
) -> std::result::Result<
    crate::agents::skills::ProviderSkillKey,
    crate::agents::skills::LaunchSettingsErr,
> {
    use crate::agents::skills::{LaunchSettingsErr, ProviderSkillKey};
    let path = skill.source.join("SKILL.md");
    let text = std::fs::read_to_string(&path).map_err(|error| LaunchSettingsErr::Settings {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    #[derive(serde::Deserialize)]
    struct Metadata {
        name: Option<String>,
    }
    // Frontmatter Codex cannot read leaves the skill unloadable there, so the
    // directory name is a harmless key rather than a reason to refuse.
    let name = crate::config::definitions::frontmatter::split(&path, &text)
        .ok()
        .and_then(|(yaml, _)| serde_saphyr::from_str::<Option<Metadata>>(yaml).ok())
        .flatten()
        .and_then(|metadata| metadata.name)
        .filter(|name| !name.trim().is_empty());
    Ok(ProviderSkillKey::new(
        name.unwrap_or_else(|| skill.name.clone()),
    ))
}

fn render_host_skills(
    keys: &[crate::agents::skills::ProviderSkillKey],
    _cwd: &Path,
    _artifact_dir: &Path,
    args: &mut Vec<String>,
) -> std::result::Result<
    Option<crate::agents::skills::LaunchSettingsArtifact>,
    crate::agents::skills::LaunchSettingsErr,
> {
    crate::agents::PresetArgMatcher::ConfigKey {
        flags: vec!["-c".into(), "--config".into()],
        key: "skills.config".into(),
    }
    .remove_occurrences(args);
    let entries = keys
        .iter()
        .map(|key| {
            format!(
                "{{name={},enabled=false}}",
                toml::Value::String(key.as_str().to_owned())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    args.extend(["-c".into(), format!("skills.config=[{entries}]")]);
    Ok(None)
}

fn render_definition_tools(
    tools: &crate::agents::ToolSet,
) -> std::result::Result<Vec<String>, crate::agents::ToolErr> {
    let bash = tools.has("Bash");
    let agent = tools.has("Agent");
    let ask = tools.has("AskUserQuestion");
    let flags = [
        ("agents.enabled", agent),
        ("features.goals", false),
        ("features.multi_agent", agent),
        ("features.multi_agent_v2", agent),
        ("features.shell_snapshot", bash),
        ("features.shell_tool", bash),
        ("features.skill_mcp_dependency_install", false),
        ("features.tool_call_mcp_elicitation", false),
        ("features.browser_use", false),
        ("features.browser_use_external", false),
        ("features.computer_use", false),
        ("features.in_app_browser", false),
        ("features.image_generation", false),
        ("features.tool_suggest", false),
        ("features.memories", false),
        ("features.default_mode_request_user_input", ask),
        ("tools.experimental_request_user_input.enabled", ask),
        ("skills.include_instructions", tools.has("Skill")),
    ];
    let web = if tools.has("WebSearch") || tools.has("WebFetch") {
        "cached"
    } else {
        "disabled"
    };
    let mut args = vec![
        "--strict-config".to_owned(),
        "-c".to_owned(),
        format!("web_search=\"{web}\""),
    ];
    for (key, value) in flags {
        args.extend(["-c".to_owned(), format!("{key}={value}")]);
    }
    Ok(args)
}

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

    fn attach_prompt_context(&self, decoded: &mut HookOutput, text: &str) -> bool {
        if decoded.event_name() != "UserPromptSubmit" {
            return false;
        }
        decoded.merge_reply_object([(
            "hookSpecificOutput".to_owned(),
            serde_json::json!({"hookEventName": "UserPromptSubmit", "additionalContext": text}),
        )]);
        true
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let hook = CodexHook::parse(event_name, payload);
        let ask_kind = match &hook {
            CodexHook::PermissionRequest(_) => Some(AskKind::Permission),
            CodexHook::PreToolUse(request) => {
                self.spec().blocking_tool_kind(request.tool_name.as_deref())
            }
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
        decoded.set_native_answers(match &hook {
            CodexHook::PostToolUse(parsed) => match (
                parsed.tool_name.as_deref(),
                parsed.tool_input.as_ref(),
                parsed.tool_response.as_ref(),
            ) {
                (Some(name), Some(input), Some(response)) => {
                    ask::answer_detail(name, input, response)
                }
                _ => None,
            },
            CodexHook::UserPromptSubmit(parsed) => parsed
                .prompt
                .as_deref()
                .and_then(ask::submitted_prompt_answer),
            _ => None,
        });
        let child_id = hook.distinct_child_id();
        let transcript = codex_transcript_observation(
            payload,
            child_id,
            matches!(hook, CodexHook::Stop(_) | CodexHook::SubagentStop(_)),
        );
        let questions = match &hook {
            CodexHook::PreToolUse(parsed) => parsed
                .tool_name
                .as_deref()
                .zip(parsed.tool_input.as_ref())
                .and_then(|(name, input)| ask::question_detail(name, input))
                .unwrap_or_default(),
            CodexHook::Stop(_) => transcript
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
        decoded.set_final_message(match &hook {
            CodexHook::Stop(stop) => stop
                .last_assistant_message
                .as_deref()
                .and_then(non_empty_trimmed),
            _ => None,
        });
        let signal = map_codex_lifecycle_signal(
            self.spec(),
            payload,
            &hook,
            transcript.turn_error.as_ref(),
            transcript.plan_proposed.is_some(),
        );
        if let Some(signal) = signal
            && let Some((agent_id, parent_agent_id)) =
                resolve_codex_observation_identity(self.spec().kind, event_name, payload, &hook)
        {
            let root_identity_event = parent_agent_id.is_none()
                && matches!(
                    signal,
                    LifecycleSignal::Registered | LifecycleSignal::TurnStarted { .. }
                );
            let compact_continuation = parent_agent_id.is_none()
                && matches!(&hook, CodexHook::SessionStart(start) if start.source == SessionSource::Compact);
            let mut observation = build_codex_observation(
                payload,
                &hook,
                signal,
                agent_id,
                parent_agent_id,
                transcript,
            );
            if (root_identity_event || compact_continuation)
                && let Some(agent_id) = observation.agent_id.as_ref()
            {
                // Only an ephemeral fork starts with a null payload path; a
                // persistent fork's path may still be flushing, so it also
                // needs the payload null before it is quarantined for good.
                observation.origin = if observation.transcript_path.is_none()
                    && matches!(&hook, CodexHook::SessionStart(start)
                        if start.source == SessionSource::Fork
                            && start.common.transcript_path.as_deref().is_none_or(str::is_empty))
                {
                    Some(SessionOrigin::SideConversation)
                } else {
                    session_origin(agent_id.as_str())
                };
                if compact_continuation {
                    observation.compacted_from = session_forked_from(agent_id.as_str());
                }
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

    fn pane_actions(&self, kind: AskKind) -> Option<&'static str> {
        (kind == AskKind::PlanApproval).then_some(ask::PLAN_PANE_ACTIONS)
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
    fn adopt_shared_file(
        &self,
        name: &str,
        existing: &Path,
        target: &Path,
    ) -> Result<Option<String>> {
        if name != "config.toml" {
            return Ok(None);
        }
        install::adopt_config(existing, target)
    }

    fn headless_requires_folder_trust(&self) -> bool {
        true
    }

    fn folder_trust(
        &self,
        cwd: &Path,
        repo_root: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<crate::agents::FolderTrust> {
        Some(match install::codex_config_path(login_env) {
            Ok(config) => project_trust::trust_gap_at(&config, cwd, repo_root),
            Err(err) => crate::agents::FolderTrust::Undecided(crate::agents::FolderTrustGap {
                path: PathBuf::new(),
                key: repo_root
                    .unwrap_or(cwd)
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.unwrap_or(cwd).to_path_buf()),
                grant: Err(format!(
                    "set CODEX_HOME to the Codex config directory: {err}"
                )),
            }),
        })
    }

    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

const SQLITE_HOME_ENV: &str = "CODEX_SQLITE_HOME";

/// Give a Codex child the account it runs for: its home, and for a shared
/// account the home its databases live in.
fn forward_login_env(command: &mut std::process::Command, login_env: &BTreeMap<String, String>) {
    command.envs(
        login_env
            .iter()
            .filter(|(key, _)| ["CODEX_HOME", SQLITE_HOME_ENV].contains(&key.as_str())),
    );
}

const MIN_NO_DAEMON: super::version::CliVersion = super::version::CliVersion::new(0, 156, 0);

impl crate::agents::capabilities::LaunchCapability for CodexAdapter {
    fn shared_home_entries(&self) -> &'static [crate::agents::capabilities::SharedHomeEntry] {
        use crate::agents::capabilities::{SharedHomeEntry, SharedHomeKind::File};
        &[
            SharedHomeEntry {
                name: "config.toml",
                kind: File,
            },
            SharedHomeEntry {
                name: "AGENTS.md",
                kind: File,
            },
        ]
    }

    fn private_home_entries(&self) -> &'static [&'static str] {
        &[
            "auth.json",
            "app-server-control",
            "app-server-daemon",
            "packages",
        ]
    }

    fn history_home_entries(&self) -> &'static [&'static str] {
        &["sessions", "archived_sessions"]
    }

    fn shared_database_home_env_key(&self) -> Option<&'static str> {
        Some(SQLITE_HOME_ENV)
    }

    fn min_version(&self) -> Option<super::version::CliVersion> {
        Some(MIN_NO_DAEMON)
    }

    fn rejected_extra_arg(
        &self,
        extra_args: &[String],
    ) -> Option<crate::agents::capabilities::RejectedLaunchArg> {
        (!crate::agents::PresetArgMatcher::Flag(vec!["--remote".to_owned()])
            .occurrences(extra_args)
            .is_empty())
        .then_some(crate::agents::capabilities::RejectedLaunchArg {
            flag: "--remote",
            reason: "managed panes run embedded (--no-daemon); remove --remote from the launch args",
        })
    }

    fn resolve_model_alias(
        &self,
        request: crate::agents::capabilities::ModelAliasRequest<'_>,
        source: Option<&mut dyn crate::agents::capabilities::ModelCatalogSource>,
    ) -> Option<crate::agents::capabilities::ModelAliasResolution> {
        model_alias::resolve(request, source)
    }

    fn known_catalog_model(
        &self,
        paths: &crate::RuntimePaths,
        login: &crate::ids::LoginKey,
        id: &str,
    ) -> Option<bool> {
        model_alias::known_model(paths, login, id)
    }
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["CODEX_HOME"]
    }

    /// Codex's shared config, credentials, and control-socket home: a
    /// non-empty `CODEX_HOME`, else `$HOME/.codex`.
    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        if let Some(raw) = env.get("CODEX_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(raw));
        }
        env.get("HOME")
            .filter(|v| !v.is_empty())
            .map(|home| PathBuf::from(home).join(".codex"))
    }

    fn manual_skill(&self) -> ManualSkill {
        ManualSkill::OpenAiPolicy
    }

    fn is_interactive_process(&self, command: &str) -> bool {
        process::is_interactive_process(command)
    }

    fn default_launch_model(&self) -> Option<String> {
        configured_model(&crate::agents::ambient_env())
    }

    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        let login_env = crate::agents::ambient_env();
        (
            configured_model(&login_env),
            configured_reasoning_effort(&login_env),
        )
    }

    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::ConfigKey {
            flags: vec!["-c".to_owned(), "--config".to_owned()],
            key: "developer_instructions".to_owned(),
        })
    }

    fn lockdown_subagent_args(&self, extra_args: &mut Vec<String>) {
        crate::agents::PresetArgMatcher::ConfigKey {
            flags: vec!["-c".to_owned(), "--config".to_owned()],
            key: "features.multi_agent".to_owned(),
        }
        .remove_occurrences(extra_args);
        extra_args.extend(["-c".to_owned(), "features.multi_agent=false".to_owned()]);
    }

    /// Yolo's bypass flag already runs commands unsandboxed, so its argv stays
    /// as rendered. `--approve-for-me` conflicts with `--sandbox` in clap, so it
    /// is replaced by the approval half of its upstream expansion.
    fn disable_native_sandbox_args(&self, extra_args: &mut Vec<String>) {
        if extra_args
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
        {
            return;
        }
        let approve_for_me_len = extra_args.len();
        extra_args.retain(|arg| arg != "--approve-for-me" && arg != "--not-so-yolo");
        if extra_args.len() != approve_for_me_len {
            extra_args.extend(
                [
                    "-c",
                    r#"approvals_reviewer="auto_review""#,
                    "-c",
                    r#"approval_policy="on-request""#,
                ]
                .map(ToOwned::to_owned),
            );
        }
        crate::agents::PresetArgMatcher::Flag(vec!["--sandbox".to_owned(), "-s".to_owned()])
            .remove_occurrences(extra_args);
        crate::agents::PresetArgMatcher::ConfigKey {
            flags: vec!["-c".to_owned(), "--config".to_owned()],
            key: "sandbox_mode".to_owned(),
        }
        .remove_occurrences(extra_args);
        extra_args.extend(["--sandbox".to_owned(), "danger-full-access".to_owned()]);
    }
}

impl crate::agents::capabilities::SessionCapability for CodexAdapter {
    fn daemon_session_evidence(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> super::session::DaemonSessionEvidence {
        let pids = codex_daemon_pids();
        let loaded_session_ids = (!pids.is_empty())
            .then(|| loaded_daemon_threads(login_env))
            .flatten();
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

    fn discover_local_sessions(
        &self,
        workspaces: &[&Path],
        login_env: &BTreeMap<String, String>,
    ) -> Vec<super::LocalSessionObservation> {
        local_sessions::discover(workspaces, login_env)
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
    fn prompt_cache_ttl(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30 * 60))
    }

    /// Codex has no statusline, so app-server-owned metadata (rate-limit
    /// windows, model display name, thread preview/name, version) refreshes
    /// out-of-band on turn boundaries: `SessionStart` populates it early (rate
    /// limits + model need no thread); `UserPromptSubmit`/`Stop` keep it
    /// current. Per-tool events are excluded — an app-server spawn per tool call
    /// is too frequent. Local transcript usage has its own stat-gated inline
    /// refresh below, where the local session index also supplies the automatic
    /// thread name.
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
        let args = crate::agents::refresh_context_argv(self.spec().kind, ctx);
        Some(RefreshSpawn { args })
    }

    /// Two sources in one pass: the local rollout tail always, and the
    /// app-server's read-only enrichment (rate-limit windows, model display
    /// name, thread name/preview, version) only when its own fields are stale.
    /// The expensive app-server read is throttled by [`app_server_due`].
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
            input.login_env,
        );
        if !app_server_due(input.prior) {
            return local.map(|local| SessionContextRefresh {
                local: Some(local),
                ..SessionContextRefresh::default()
            });
        }
        let observation = refresh_app_server_enrichment(
            Some(input.session_id),
            input.model,
            input.broker_socket,
            input.login_env,
        );
        let realtime_usage = observation
            .as_ref()
            .map(AppServerObservation::account_usage);
        Some(SessionContextRefresh {
            local,
            observed: observation.map(|observation| observation.context),
            realtime_usage,
        })
    }

    fn merge_session_context(
        &self,
        record: &mut crate::agents::context::record::AgentContextRecord,
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
        refresh_local_context_under(ctx, self.config_home(ctx.login_env).as_deref())
    }
}

fn refresh_local_context_under(
    ctx: &LocalContextRefreshCtx<'_>,
    codex_home: Option<&Path>,
) -> Option<LocalContextRefresh> {
    let mut refresh = refresh_transcript_context(
        ctx.agent_id,
        ctx.model_hint,
        ctx.prior_transcript_path,
        ctx.prior_transcript_stat,
        ctx.prior_spend_fold,
        ctx.shared_pricing_cache_path,
        ctx.login_env,
    );
    let session_name = codex_home
        .and_then(|home| session_index::session_name_under(home, ctx.agent_id))
        .filter(|name| Some(name.as_str()) != ctx.prior_session_name);
    let Some(session_name) = session_name else {
        return refresh;
    };
    let enriched = refresh.get_or_insert_with(|| LocalContextRefresh {
        transcript_path: ctx.prior_transcript_path.map(str::to_owned),
        transcript_stat: ctx.prior_transcript_stat.copied(),
        ..LocalContextRefresh::default()
    });
    enriched.context.session_name = FieldPatch::Set(session_name);
    refresh
}

impl crate::agents::capabilities::AccountCapability for CodexAdapter {
    fn prepare_reset_credit(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> std::result::Result<super::account::ResetCreditOffer, String> {
        let (credentials, base_url) = oauth_usage::load_configured_credentials(login_env)
            .map_err(|error| error.to_string())?;
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

    fn probe_account(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> crate::agents::account::AccountProbe {
        account::probe(login_env)
    }

    fn probe_account_usage(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> crate::agents::AccountUsageProbe {
        oauth_usage::probe_usage(login_env)
    }

    fn probe_realtime_account_usage(
        &self,
        runtime: &crate::RuntimePaths,
        login_env: &BTreeMap<String, String>,
    ) -> Option<AccountUsageSnapshot> {
        refresh_app_server_enrichment(
            None,
            None,
            Some(&runtime.codex_app_server_socket_path()),
            login_env,
        )
        .map(|observation| observation.account_usage())
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
    fn spending_sources(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<crate::agents::spending::SpendingSource> {
        spend::codex_homes(login_env)
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
                    .filtered("codex-legacy-v3", spend::legacy_spend_relative)
                    .descend_filtered("codex-legacy-dirs-v3", spend::legacy_spend_relative);
                Some(crate::agents::spending::SpendingSource::group(vec![
                    active, archived, legacy,
                ]))
            })
            .collect()
    }

    fn session_transcript(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        find_session_transcript(session_id, login_env)
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
        login_env: &BTreeMap<String, String>,
    ) -> super::runtime_control::RuntimeControlReadiness {
        app_server::daemon::readiness(enabled, login_env)
    }

    fn ensure_runtime_control(&self, enabled: bool, login_env: &BTreeMap<String, String>) {
        app_server::daemon::ensure(enabled, login_env);
    }

    fn reconcile_runtime_control(
        &self,
        enabled: bool,
        login_env: &BTreeMap<String, String>,
    ) -> std::result::Result<(), super::runtime_control::RuntimeControlError> {
        app_server::daemon::reconcile(enabled, login_env)
            .map_err(|error| super::runtime_control::RuntimeControlError::new("codex", error))
    }

    fn runtime_control_advisory(&self, login_env: &BTreeMap<String, String>) -> Option<String> {
        app_server::daemon::updater_skew(login_env).map(|skew| skew.to_string())
    }

    fn runtime_control_writes_history(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> super::runtime_control::DaemonSessions {
        app_server::daemon::writes_history(login_env)
    }
}

/// One Codex hook payload, parsed once by event name: exactly one typed
/// payload, or none for `Interrupt` and names this adapter does not type.
enum CodexHook {
    SessionStart(CodexSessionStart),
    UserPromptSubmit(CodexUserPromptSubmit),
    SubagentStart(CodexSubagentStart),
    SubagentStop(CodexSubagentStop),
    PreToolUse(CodexPreToolUse),
    PermissionRequest(CodexPermissionRequest),
    PostToolUse(CodexPostToolUse),
    PreCompact(CodexPreCompact),
    PostCompact(CodexPostCompact),
    Stop(CodexStop),
    Interrupt,
    Other,
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

impl CodexHook {
    fn parse(event_name: &str, payload: &Value) -> Self {
        match event_name {
            "SessionStart" => Self::SessionStart(payloads::parse(payload)),
            "UserPromptSubmit" => Self::UserPromptSubmit(payloads::parse(payload)),
            "SubagentStart" => Self::SubagentStart(payloads::parse(payload)),
            "SubagentStop" => Self::SubagentStop(payloads::parse(payload)),
            "PreToolUse" => Self::PreToolUse(payloads::parse(payload)),
            "PermissionRequest" => Self::PermissionRequest(payloads::parse(payload)),
            "PostToolUse" => Self::PostToolUse(payloads::parse(payload)),
            "PreCompact" => Self::PreCompact(payloads::parse(payload)),
            "PostCompact" => Self::PostCompact(payloads::parse(payload)),
            "Stop" => Self::Stop(payloads::parse(payload)),
            "Interrupt" => Self::Interrupt,
            _ => Self::Other,
        }
    }

    fn child(&self) -> Option<CodexChild<'_>> {
        let (identity, common) = match self {
            Self::SubagentStart(p) => (&p.child, &p.common),
            Self::SubagentStop(p) => (&p.child, &p.common),
            Self::UserPromptSubmit(p) => (&p.child, &p.common),
            Self::PreToolUse(p) => (&p.child, &p.common),
            Self::PermissionRequest(p) => (&p.child, &p.common),
            Self::PostToolUse(p) => (&p.child, &p.common),
            Self::PreCompact(p) => (&p.child, &p.common),
            Self::PostCompact(p) => (&p.child, &p.common),
            Self::SessionStart(_) | Self::Stop(_) | Self::Interrupt | Self::Other => return None,
        };
        Some(CodexChild { identity, common })
    }

    fn distinct_child_id(&self) -> Option<&str> {
        let child = self.child()?;
        child
            .is_distinct()
            .then_some(child.identity.agent_id.as_deref())
            .flatten()
    }

    fn hook_model(&self) -> Option<String> {
        match self {
            Self::SessionStart(session) => session.common.model.clone(),
            _ => self.child()?.common.model.clone(),
        }
    }
}

fn map_codex_lifecycle_signal(
    spec: &AgentSpec,
    payload: &Value,
    hook: &CodexHook,
    turn_error: Option<&AgentTurnError>,
    plan_proposed: bool,
) -> Option<LifecycleSignal> {
    let errored = || stop_payload_errored(payload) || turn_error.is_some();
    let awaiting = |kind| LifecycleSignal::AwaitingInput {
        kind,
        ask_id: None,
        detail: None,
        native_key: None,
    };
    Some(match hook {
        CodexHook::SessionStart(start) => start.source.session_start_signal(),
        CodexHook::SubagentStart(_) => LifecycleSignal::SubagentStarted,
        CodexHook::UserPromptSubmit(_) => LifecycleSignal::TurnStarted { turn_id: None },
        CodexHook::SubagentStop(_) => LifecycleSignal::SubagentStopped { errored: errored() },
        CodexHook::Stop(_) if plan_proposed => awaiting(AskKind::PlanApproval),
        CodexHook::Stop(_) => LifecycleSignal::TurnEnded {
            errored: errored(),
            parked_on_background: false,
            turn_id: None,
        },
        CodexHook::PermissionRequest(_) => awaiting(AskKind::Permission),
        CodexHook::PostToolUse(tool) => LifecycleSignal::ToolUsed {
            mutates: spec.tool_mutates(payload),
            edits: spec.tool_edits_files(payload),
            name: tool.tool_name.clone(),
            native_key: None,
            turn_id: tool.common.turn_id.clone(),
        },
        CodexHook::PreToolUse(tool) => match spec.blocking_tool_kind(tool.tool_name.as_deref()) {
            Some(kind) => awaiting(kind),
            None => LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: None,
                native_key: None,
                turn_id: tool.common.turn_id.clone(),
            },
        },
        CodexHook::PreCompact(_) => LifecycleSignal::Compacting,
        CodexHook::PostCompact(compact) => LifecycleSignal::CompactionEnded {
            auto: compact.trigger.auto_flag(),
            failed: false,
        },
        CodexHook::Interrupt => LifecycleSignal::TurnInterrupted {
            turn_id: optional_payload_string(payload, &["turn_id"]),
        },
        CodexHook::Other => return None,
    })
}

fn codex_transcript_path(payload: &Value) -> Option<PathBuf> {
    optional_payload_string(payload, &["transcript_path"])
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            optional_payload_string(payload, &["session_id"])
                .and_then(|id| find_session_transcript(&id, &crate::agents::ambient_env()))
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
            find_session_transcript(child_id, &crate::agents::ambient_env()).map(|path| {
                CodexChildTranscript {
                    path,
                    validated_header: None,
                }
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
    hook: &CodexHook,
) -> Option<ObservationIdentity> {
    let child = hook.child();
    let subagent_event = matches!(
        hook,
        CodexHook::SubagentStart(_) | CodexHook::SubagentStop(_)
    ) || child.as_ref().is_some_and(CodexChild::is_distinct);
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
    hook: &CodexHook,
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
        .then(|| hook.child()?.identity.agent_type.clone())
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
    let user_prompt = match hook {
        CodexHook::UserPromptSubmit(prompt) => Some(prompt),
        _ => None,
    };
    observation.prompt = SanitizedPrompt::new(user_prompt.and_then(|p| p.prompt.as_deref()));
    observation.ask_queue = match hook {
        CodexHook::PostToolUse(tool) => ask::queued_questions(tool),
        CodexHook::UserPromptSubmit(p) => p.prompt.as_deref().and_then(ask::answered_questions),
        _ => None,
    };
    if user_prompt.is_some() && observation.ask_queue.is_some() {
        observation.prompt = None;
        observation.task = None;
    }
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
    let login_env = crate::agents::ambient_env();
    observation.launch.model = hook
        .hook_model()
        .or_else(|| optional_payload_string(payload, &["model"]))
        .or(usage.model)
        .or_else(|| is_subagent.then(|| configured_model(&login_env)).flatten());
    observation.launch.effort = payload_reasoning_effort(payload)
        .or(usage_effort)
        .or_else(|| {
            is_subagent
                .then(|| configured_reasoning_effort(&login_env))
                .flatten()
        });
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
fn refresh_app_server_enrichment(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
    login_env: &BTreeMap<String, String>,
) -> Option<AppServerObservation> {
    let mut client = CodexAppServer::connect(broker_socket, login_env, None)?;
    Some(client.observe("codex", session_id, model_hint, Timestamp::now()))
}

/// The thread ids the per-user Codex app-server daemon currently holds in memory,
/// for the sidebar's daemon-mode ghost reap
/// ([`crate::store::snapshot::SidebarSnapshot::reap_runtime`]).
/// Connects to the daemon **specifically** — never a cold-spawn, whose empty set
/// would mass-reap — and reads `thread/loaded/list`. `None` when there is no daemon
/// to ask or its list cannot be trusted, which the caller reads as "unknown, keep
/// all". Spawned out-of-band by the sidebar producer; read-only, best-effort.
fn loaded_daemon_threads(
    login_env: &BTreeMap<String, String>,
) -> Option<std::collections::BTreeSet<String>> {
    let mut client = CodexAppServer::connect_daemon(login_env)?;
    let ids = client.loaded_threads().ok()?;
    Some(ids.into_iter().collect())
}

#[cfg(test)]
mod tests;
