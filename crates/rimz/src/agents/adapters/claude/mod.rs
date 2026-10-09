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
//! `docs/internals/agents/adapter_claude.md`). The `PreToolUse` blocking sub-events ride the
//! broad `PreToolUse` hook and self-classify from `tool_name`.

mod account;
mod ask;
mod folder_trust;
mod headless;
mod install;
mod json_edit;
mod local_context;
mod local_sessions;
mod managed_pricing;
pub(in crate::agents) mod oauth_usage;
mod payloads;
mod remote_consent;
mod remote_control;
mod remote_liveness;
mod spend;
mod statusline;
mod subagent_cost;
mod subagent_statusline;
mod subagents;

pub(crate) use crate::agents::capabilities::*;
use std::collections::BTreeMap;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use self::install::MANAGED_SOURCE;
use self::payloads::{
    ClaudeCommon, ClaudePermissionRequest, ClaudePostCompact, ClaudePostToolUse, ClaudePreToolUse,
    ClaudeSessionStart, ClaudeStop, ClaudeStopFailure, ClaudeSubagentStart, ClaudeSubagentStop,
    ClaudeUserPromptSubmit, parse,
};
use super::AskKind;
use super::RemoteControlStatus;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookContextReply, HookCoverage, LifecycleAnnotations, PlanLabel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{BackgroundTask, HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::observation::{payload_has_context_observation, payload_total_tokens};
use super::pricing::PriceBook;
use super::{
    AgentHookClass, AgentLifecycleObservation, AgentTurnError, BackgroundShell,
    BackgroundShellReport, HookOutput, HookRouting, Result, RootIdentity, SanitizedPrompt,
    SessionOrigin, SpawnedSubagent, SubagentIdentity, SubagentObservation, SubagentSpawnInput,
    TranscriptMessage, non_empty_trimmed, optional_payload_string, read_transcript_tail,
    resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
use crate::agents::payload::finished_task_notification_ids;
use crate::agents::{TurnSettle, TurnSettleOutcome};
use crate::transcript::AskQuestion;

/// Everything `const` about Claude Code, in one place. See
/// [`AgentSpec`] for the spec-vs-trait split.
static CLAUDE_DESCRIPTOR: AgentSpec = AgentSpec {
    tool_rules: crate::agents::skills::ToolRules::Settings {
        render: render_tool_rules,
    },
    host_skills: crate::agents::skills::HostSkills::Switch {
        flag: "--settings skillOverrides",
        effect: "user-only",
        key: |skill| {
            Ok(crate::agents::skills::ProviderSkillKey::new(
                skill.name.clone(),
            ))
        },
        render: render_host_skills,
    },
    kind: "claude",
    aliases: &[],
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
        input_key: Some("tool_input"),
        mutating: &["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"],
        editing: &["Edit", "Write", "MultiEdit", "NotebookEdit"],
        blocking: &[
            ("ExitPlanMode", AskKind::PlanApproval),
            ("AskUserQuestion", AskKind::Question),
        ],
    },
    capabilities: Capabilities {
        hook_context: Some(HookContextReply::HookSpecificOutput { event_name: true }),
        prompt_context: true,
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
        remote_control: RemoteControlCapability {
            pane_sessions: true,
            background_sessions: true,
        },
    },
    coverage: CLAUDE_COVERAGE,
    user_coverage: CLAUDE_USER_COVERAGE,
    lifecycle_hooks: CLAUDE_LIFECYCLE_HOOKS,
    default_context_window: Some(200_000),
    default_model: None,
    process_names: &["claude"],
    bin_names: &["claude"],
    bin_identity: None,
    extra_bin_dirs: &[],
    // A Claude session spreads across `<session_id>/chat.jsonl` plus
    // `<session_id>/subagents/*.jsonl`; the session directory is the thread.
    thread_key: ThreadKey::SessionDir,
    launch: super::LaunchSpec {
        headless: Some(&headless::ClaudeHeadless),
        definitions: DEFINITIONS,
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
        max_turn_flag: Some("--max-turns"),
        interrupt_key: Some(crate::pane::keys::NamedKey::Escape),
        compact_command: Some(super::CompactCommand {
            command: "/compact",
            instruction: super::CompactInstruction::Trailing,
        }),
        presets: super::PresetMatchers {
            auto_compact: Some(super::StaticPresetMatcher::Flag(&["--autocompact"])),
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: Some(super::StaticPresetMatcher::Flag(&["--system-prompt-file"])),
        },
    },
};

const CLAUDE_COVERAGE: CoverageAnnotations = CoverageAnnotations {
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
        via: "SubagentStart/SubagentStop + interrupt-derived close",
    },
    launch_reminders: ConcernCoverage::Wired {
        via: "--append-system-prompt",
    },
    background_parking: ConcernCoverage::Wired {
        via: "Stop.background_tasks/session_crons",
    },
    background_shells: ConcernCoverage::Wired {
        via: "PostToolUse Bash backgroundTaskId + Stop.background_tasks[type=shell] + task-notification prompt",
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
    tool_stats: ConcernCoverage::Wired {
        via: "hook tool names + transcript tool_use blocks",
    },
    remote_control: ConcernCoverage::Wired {
        via: "pane/background",
    },
};

const CLAUDE_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card opens at session start, follows every turn, and clears when Claude exits",
    },
    live: CapabilityLevel::Full {
        note: "Claude's statusline keeps context fill, the token split, and the dollar current mid-turn",
    },
    history: CapabilityLevel::Full {
        note: "every past session reads end to end, each turn priced for stats and the dashboard",
    },
    account: CapabilityLevel::Full {
        note: "login, plan, and the 5h and 7d windows with their fill, reset, and extra credits",
    },
    ask: CapabilityLevel::Full {
        note: "permissions, plans, and questions raise Waiting and reach rimz asks with their options",
    },
    subagents: CapabilityLevel::Full {
        note: "Task children nest under the parent as they start, with task, model, and tokens",
    },
};

const CLAUDE_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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

/// Per-hook timeout written into the Claude config (seconds). Hooks append
/// ingress and nudge the elected drainer; reply-bearing hooks wait within
/// this budget.
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 2;

/// Installed events and classification policy. RimZ installs every event as a
/// single broad hook with no matcher: the helper classifies
/// each call from the payload's `tool_name`, so `PreToolUse: ExitPlanMode` and
/// `PreToolUse: AskUserQuestion` still route to their blocking ask kinds off
/// the broad `PreToolUse` hook. A dedicated `ExitPlanMode|AskUserQuestion`
/// matcher would only double-fire — Claude runs every matching matcher group,
/// and the broad entry already matches those tools. The broad
/// `PreToolUse`/`PostToolUse` hooks also keep the sidebar's enrichment current.
/// The matcher field stays explicit because the reclaim path still reasons
/// about on-disk matchers left by users or older builds. Every sample payload
/// mirrors the shipped wire, so the `PreToolUse` and `PostToolUse` entries
/// carry the `tool_use_id` that keys their signals.
const CLAUDE_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "SessionStart",
        r#"{"session_id":"sess-1","source":"startup"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("SessionEnd", r#"{"session_id":"sess-1"}"#).session_ended(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"session_id":"sess-1","prompt":"fix auth"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("Stop", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle(
        "StopFailure",
        r#"{"session_id":"sess-1","error":"api_error"}"#,
    ),
    HookEventSpec::lifecycle("Notification", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::blocking(
        "PermissionRequest",
        r#"{"session_id":"sess-1","tool_name":"Bash"}"#,
        AskKind::Permission,
    )
    .synchronous()
    .with_lifecycle_fallback(),
    HookEventSpec::lifecycle(
        "PreToolUse",
        r#"{"session_id":"sess-1","tool_name":"Bash","tool_use_id":"toolu_bash"}"#,
    ),
    HookEventSpec::lifecycle(
        "PostToolUse",
        r#"{"session_id":"sess-1","tool_name":"Edit","tool_use_id":"toolu_edit"}"#,
    )
    .progress(),
    // Subagent lifecycle (Claude Code's Task-tool children, parity with Codex's
    // threads): `SubagentStart` registers a child row keyed by its `agent_id`,
    // `SubagentStop` returns it to idle. Both carry the parent root `session_id`.
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"session_id":"sess-parent","agent_id":"child-1","subagent_type":"Explore"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"session_id":"sess-parent","agent_id":"child-1","agent_type":"Explore"}"#,
    )
    .progress(),
    // Fires around context compaction (manual `/compact` or auto). Pre opens
    // the transient compacting head; Post carries the trigger bit when present,
    // while SessionStart(source=compact) is the reliable triggerless closer.
    HookEventSpec::lifecycle("PreCompact", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle(
        "PostCompact",
        r#"{"session_id":"sess-1","trigger":"manual"}"#,
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
pub(in crate::agents) struct ClaudeAdapter;

const DEFINITIONS: crate::agents::definition::DefinitionSpec =
    crate::agents::definition::DefinitionSpec {
        mode: Some(crate::agents::PermissionMode::Auto),
        effort: Some("xhigh"),
        models: &[
            crate::agents::definition::DefinitionModel {
                name: "opus",
                id: "opus",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "sonnet",
                id: "sonnet",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "haiku",
                id: "haiku",
                effort: None,
            },
            crate::agents::definition::DefinitionModel {
                name: "fable",
                id: "fable",
                effort: Some("high"),
            },
        ],
        prefixes: &["claude-"],
        tools: crate::agents::definition::DefinitionTools::Required(render_definition_tools),
    };

const CLAUDE_AGENT_TYPES: &[&str] = &[
    "Explore",
    "Plan",
    "general-purpose",
    "statusline-setup",
    "fork",
];

fn render_host_skills(
    keys: &[crate::agents::skills::ProviderSkillKey],
    cwd: &Path,
    artifact_dir: &Path,
    args: &mut Vec<String>,
) -> std::result::Result<
    Option<crate::agents::skills::LaunchSettingsArtifact>,
    crate::agents::skills::LaunchSettingsErr,
> {
    merge_settings(cwd, artifact_dir, args, None, true, |object| {
        let overrides = object
            .entry("skillOverrides")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| "skillOverrides must be an object".to_owned())?;
        for key in keys {
            overrides.insert(
                key.as_str().to_owned(),
                serde_json::json!("user-invocable-only"),
            );
        }
        Ok(())
    })
}

fn merge_settings(
    cwd: &Path,
    artifact_dir: &Path,
    args: &mut Vec<String>,
    pending: Option<&crate::agents::skills::LaunchSettingsArtifact>,
    private_inline: bool,
    merge: impl FnOnce(
        &mut serde_json::Map<String, serde_json::Value>,
    ) -> std::result::Result<(), String>,
) -> std::result::Result<
    Option<crate::agents::skills::LaunchSettingsArtifact>,
    crate::agents::skills::LaunchSettingsErr,
> {
    use crate::agents::skills::LaunchSettingsErr;
    let matcher = crate::agents::PresetArgMatcher::Flag(vec!["--settings".into()]);
    let value = matcher
        .occurrences(args)
        .into_iter()
        .last()
        .map(|item| item.value);
    let path = value
        .as_deref()
        .filter(|value| !value.trim_start().starts_with('{'))
        .map(|value| cwd.join(value))
        .unwrap_or_else(|| PathBuf::from("--settings"));
    let source_path = pending
        .filter(|(pending_path, _, _)| *pending_path == path)
        .map(|(_, _, source)| source)
        .unwrap_or(&path)
        .clone();
    let invalid = |reason: String| LaunchSettingsErr::Settings {
        path: source_path.clone(),
        reason,
    };
    let private = value
        .as_ref()
        .is_some_and(|value| private_inline || !value.trim_start().starts_with('{'));
    let mut settings: serde_json::Value = match (value, pending) {
        (Some(value), Some((path, settings, _))) if path == Path::new(&value) => settings.clone(),
        (Some(value), _) => {
            let bytes = if value.trim_start().starts_with('{') {
                value.into_bytes()
            } else {
                std::fs::read(&path).map_err(|error| invalid(error.to_string()))?
            };
            crate::agents::jsonc::from_slice(&bytes)
                .map_err(|error| invalid(format!("invalid JSON: {error}")))?
        }
        (None, _) => serde_json::json!({}),
    };
    let object = settings
        .as_object_mut()
        .ok_or_else(|| invalid("expected a JSON object".into()))?;
    merge(object).map_err(invalid)?;
    matcher.remove_occurrences(args);
    if private {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(settings.to_string().as_bytes()));
        let path = artifact_dir.join(format!("settings.{digest}.json"));
        args.extend(["--settings".into(), path.display().to_string()]);
        return Ok(Some((path, settings, source_path)));
    }
    args.extend(["--settings".into(), settings.to_string()]);
    Ok(None)
}

const ROUTINE_RIMZ_PREFIXES: &[&str] = &[
    "rimz message",
    "rimz agents",
    "rimz subagents",
    "rimz teams",
    "rimz asks",
    "rimz answer",
    "rimz wait --in",
    "rimz pane list",
    "rimz pane capture",
    "rimz loop show",
    "rimz loop logs",
    "rimz lsp def",
    "rimz lsp refs",
    "rimz lsp hover",
    "rimz lsp impl",
    "rimz lsp callers",
    "rimz lsp callees",
    "rimz lsp symbols",
    "rimz lsp find",
    "rimz lsp check",
    "rimz lsp list",
    "rimz lsp status",
];

fn union_settings_array(
    object: &mut serde_json::Map<String, serde_json::Value>,
    section: &str,
    key: &str,
    additions: impl IntoIterator<Item = String>,
) -> std::result::Result<(), String> {
    let section_value = object
        .entry(section)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{section} must be an object"))?;
    let values = section_value
        .entry(key)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("{section}.{key} must be an array"))?;
    values.extend(additions.into_iter().map(serde_json::Value::String));
    let mut unique = Vec::new();
    values.retain(|value| {
        if unique.contains(value) {
            return false;
        }
        unique.push(value.clone());
        true
    });
    Ok(())
}

fn render_tool_rules(
    rules: &[crate::config::ToolRule],
    (cwd, artifact_dir): (&Path, &Path),
    extra_args: &mut Vec<String>,
    artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
    use crate::agents::skills::LaunchSettingsErr;
    if rules.is_empty() {
        return Ok(());
    }
    *artifact = merge_settings(
        cwd,
        artifact_dir,
        extra_args,
        artifact.as_ref(),
        false,
        |object| {
            union_settings_array(
                object,
                "permissions",
                "allow",
                rules.iter().map(ToString::to_string),
            )
        },
    )
    .map_err(|error| match error {
        LaunchSettingsErr::Settings { path, reason } => LaunchSettingsErr::Settings {
            path,
            reason: format!(
                "{reason}; correct that key, or remove the definition's allowed-tools list"
            ),
        },
        error => error,
    })?;
    Ok(())
}

fn render_definition_tools(
    tools: &crate::agents::ToolSet,
) -> std::result::Result<Vec<String>, crate::agents::ToolErr> {
    for name in tools.agent_types() {
        if !CLAUDE_AGENT_TYPES.contains(&name.as_str()) {
            return Err(crate::agents::ToolErr::UnknownAgent {
                name: name.clone(),
                known: CLAUDE_AGENT_TYPES.join(", "),
            });
        }
    }
    let mut args = vec!["--strict-mcp-config".to_owned()];
    let denied: Vec<_> = CLAUDE_AGENT_TYPES
        .iter()
        .filter(|name| !tools.agent_types().iter().any(|allowed| allowed == **name))
        .collect();
    if !tools.agent_types().is_empty() && !denied.is_empty() {
        args.push("--disallowedTools".to_owned());
        args.extend(denied.into_iter().map(|name| format!("Agent({name})")));
    }
    args.extend(["--tools".to_owned(), tools.bases().join(",")]);
    Ok(args)
}

fn hook_ingress_decision(
    pid: Option<u32>,
    spawned_by_remote_control: bool,
) -> super::HookIngressDecision {
    if spawned_by_remote_control {
        super::HookIngressDecision::Ignore(super::HookIngressIgnoreReason::ClaudeRemoteControl)
    } else {
        super::HookIngressDecision::Accept(super::HookIngressAcceptance::agent(pid))
    }
}

impl crate::agents::capabilities::CoreCapability for ClaudeAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &CLAUDE_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
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
                serde_json::json!({
                    "session_id": "sess-1",
                    "tool_name": "ExitPlanMode",
                    "tool_use_id": "toolu_plan",
                }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::PlanApproval),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({
                    "session_id": "sess-1",
                    "tool_name": "AskUserQuestion",
                    "tool_use_id": "toolu_question",
                }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "PostToolUse",
                serde_json::json!({
                    "session_id": "sess-1",
                    "tool_name": "Bash",
                    "tool_input": { "command": "cargo test", "run_in_background": true },
                    "tool_response": { "backgroundTaskId": "b1" },
                    "tool_use_id": "toolu_bg",
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]);
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "sess-1",
                file_name: "chat.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"timestamp":"2026-06-02T10:00:00.000Z","sessionId":"sess-1","costUSD":0.42,"requestId":"req-1","message":{"id":"msg-1","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
                ),
            }),
            local_session: Some(local_sessions::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::LaunchCapability for ClaudeAdapter {
    fn shared_home_entries(&self) -> &'static [crate::agents::capabilities::SharedHomeEntry] {
        use crate::agents::capabilities::{
            SharedHomeEntry,
            SharedHomeKind::{Dir, File},
        };
        &[
            SharedHomeEntry {
                name: "settings.json",
                kind: File,
            },
            SharedHomeEntry {
                name: "settings.local.json",
                kind: File,
            },
            SharedHomeEntry {
                name: "CLAUDE.md",
                kind: File,
            },
            SharedHomeEntry {
                name: "skills",
                kind: Dir,
            },
            SharedHomeEntry {
                name: "plugins",
                kind: Dir,
            },
            SharedHomeEntry {
                name: "agents",
                kind: Dir,
            },
            SharedHomeEntry {
                name: "commands",
                kind: Dir,
            },
            SharedHomeEntry {
                name: "output-styles",
                kind: Dir,
            },
        ]
    }

    fn private_home_entries(&self) -> &'static [&'static str] {
        &[
            ".credentials.json",
            ".claude.json",
            ".oauth_refresh.lock",
            ".last-update-result.json",
        ]
    }

    fn history_home_entries(&self) -> &'static [&'static str] {
        &["projects"]
    }

    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["CLAUDE_CONFIG_DIR"]
    }

    fn temp_dir_env_keys(&self) -> &'static [&'static str] {
        &["CLAUDE_CODE_TMPDIR"]
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        remote_consent::configured_dir(env.get("CLAUDE_CONFIG_DIR").map(String::as_str)).or_else(
            || {
                env.get("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".claude"))
            },
        )
    }

    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        Some(self.config_home(env)?.join("skills"))
    }

    fn manual_skill(&self) -> ManualSkill {
        ManualSkill::Frontmatter
    }

    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::TextFlag {
            flags: vec!["--append-system-prompt".to_owned()],
        })
    }

    fn lockdown_subagent_args(&self, extra_args: &mut Vec<String>) {
        deny_native_tool(extra_args, "Agent");
    }

    fn allow_routine_rimz_args(
        &self,
        (cwd, artifact_dir): (&Path, &Path),
        skill_roots: (Option<&Path>, Option<&Path>),
        dirs: &[PathBuf; 2],
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        use crate::agents::skills::LaunchSettingsErr;
        let skills = crate::agents::skills::enumerate(skill_roots.0, skill_roots.1, |name| {
            name.starts_with("rimz-") && !name.contains('*')
        })
        .map_err(LaunchSettingsErr::RoutineSkills)?;
        let [tmp, shared] = dirs.each_ref().map(|path| path.display().to_string());
        let allow = ROUTINE_RIMZ_PREFIXES
            .iter()
            .map(|prefix| format!("Bash({prefix} *)"))
            .chain(skills.keys().map(|name| format!("Skill({name})")));
        let environment = format!(
            "rimz is this machine's agent-coordination CLI. These subcommands are routine \
             coordination: {}. $TMPDIR ({tmp}) is this agent's temporary directory, including \
             for redirected command output; $RIMZ_SHARED ({shared}) holds files a peer or \
             teammate must read.",
            ROUTINE_RIMZ_PREFIXES.join(", "),
        );
        let merged = merge_settings(
            cwd,
            artifact_dir,
            extra_args,
            artifact.as_ref(),
            false,
            |object| {
                union_settings_array(object, "permissions", "allow", allow)?;
                union_settings_array(
                    object,
                    "permissions",
                    "additionalDirectories",
                    [tmp.clone(), shared.clone()],
                )?;
                union_settings_array(
                    object,
                    "autoMode",
                    "environment",
                    ["$defaults".to_owned(), environment],
                )
            },
        );
        *artifact = merged.map_err(|error| match error {
            LaunchSettingsErr::Settings { path, reason } => LaunchSettingsErr::Settings {
                path,
                reason: format!("{reason}; correct that key, or set allow-routine-rimz = false"),
            },
            error => error,
        })?;
        Ok(())
    }

    fn allow_listed_skill_args(
        &self,
        listed: &[crate::config::SkillName],
        (cwd, artifact_dir): (&Path, &Path),
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        use crate::agents::skills::LaunchSettingsErr;
        let allow: Vec<_> = listed
            .iter()
            .filter(|name| !name.as_str().contains('*'))
            .map(|name| format!("Skill({name})"))
            .collect();
        if allow.is_empty() {
            return Ok(());
        }
        *artifact = merge_settings(
            cwd,
            artifact_dir,
            extra_args,
            artifact.as_ref(),
            false,
            |object| union_settings_array(object, "permissions", "allow", allow),
        )
        .map_err(|error| match error {
            LaunchSettingsErr::Settings { path, reason } => LaunchSettingsErr::Settings {
                path,
                reason: format!("{reason}; correct that key, or remove the profile's skills list"),
            },
            error => error,
        })?;
        Ok(())
    }

    fn disable_native_lsp_args(&self, extra_args: &mut Vec<String>) {
        deny_native_tool(extra_args, "LSP");
    }
}

fn deny_native_tool(extra_args: &mut Vec<String>, denied_tool: &str) {
    const FLAGS: [&str; 2] = ["--disallowedTools", "--disallowed-tools"];

    let mut denied = Vec::new();
    let mut retained = Vec::with_capacity(extra_args.len());
    let mut index = 0;
    while index < extra_args.len() {
        let arg = &extra_args[index];
        if FLAGS.contains(&arg.as_str()) {
            index += 1;
            while index < extra_args.len() && !extra_args[index].starts_with('-') {
                denied.push(extra_args[index].clone());
                index += 1;
            }
            continue;
        }
        if let Some(value) = FLAGS.iter().find_map(|flag| {
            arg.strip_prefix(flag)
                .and_then(|suffix| suffix.strip_prefix('='))
        }) {
            denied.push(value.to_owned());
            index += 1;
            continue;
        }
        retained.push(arg.clone());
        index += 1;
    }

    let mut unique_denied = Vec::new();
    for tool in denied {
        if tool != denied_tool
            && !tool
                .strip_prefix(denied_tool)
                .is_some_and(|suffix| suffix.starts_with('('))
            && !unique_denied.contains(&tool)
        {
            unique_denied.push(tool);
        }
    }
    unique_denied.push(denied_tool.to_owned());
    retained.push("--disallowedTools".to_owned());
    retained.extend(unique_denied);
    *extra_args = retained;
}

impl crate::agents::capabilities::HookCapability for ClaudeAdapter {
    fn hook_ingress(&self, pid: Option<u32>) -> super::HookIngressDecision {
        hook_ingress_decision(pid, remote_control::spawned_by_remote_control())
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
        let hook = ClaudeHook::parse(event_name, payload);
        let signal = map_claude_lifecycle_signal(self.spec(), payload, &hook);
        let ask_kind = match &signal {
            Some(LifecycleSignal::AwaitingInput { kind, .. }) => Some(*kind),
            _ => None,
        };
        // Cursor can execute Claude-compatible third-party hook commands with
        // Cursor-shaped payloads. Drop those before they can double-record or
        // be misparsed; `cursor_version` is Cursor's common-input discriminator.
        let mut decoded = if payload.get("cursor_version").is_some() {
            HookOutput::new(super::ClassifiedHook {
                class: AgentHookClass::Unknown,
                ask_kind: None,
                event_name: event_name.to_owned(),
            })
        } else {
            decode_catalog_hook(CLAUDE_HOOKS, event_name, ask_kind)
        };
        decoded.set_routing(
            HookRouting::split(
                optional_payload_string(payload, &["agent_id", "session_id"]).map(Into::into),
                optional_payload_string(payload, &["session_id", "agent_id"]).map(Into::into),
            )
            .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        let questions = match &hook {
            ClaudeHook::PreToolUse(parsed) => parsed
                .tool_name
                .as_deref()
                .zip(parsed.tool_input.as_ref())
                .and_then(|(name, input)| ask::question_detail(name, input))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let ask_detail = if matches!(hook, ClaudeHook::PermissionRequest(_)) {
            super::question::permission_detail(payload)
        } else {
            questions
                .first()
                .and_then(|question| question.question.lines().next())
                .map(ToOwned::to_owned)
                .filter(|detail| !detail.is_empty())
        };
        decoded.set_ask(questions, ask_detail);
        decoded.set_native_answers(match &hook {
            ClaudeHook::PostToolUse(parsed) => parsed
                .tool_name
                .as_deref()
                .zip(parsed.tool_response.as_ref())
                .and_then(|(name, response)| ask::answer_detail(name, response)),
            _ => None,
        });
        let terminal_tail = matches!(hook, ClaudeHook::Stop(_))
            .then(|| transcript_tail_from_payload(payload))
            .flatten();
        let turn_error = match &hook {
            ClaudeHook::StopFailure(parsed) => Some(parsed),
            _ => None,
        }
        .and_then(|parsed| {
            let error = parsed.error.as_deref()?.trim();
            if error.is_empty() {
                return None;
            }
            let label = parsed
                .last_assistant_message
                .as_deref()
                .and_then(crate::agents::context::cap_turn_error_label);
            Some(AgentTurnError {
                class: statusline::classify_api_error(Some(error), None, label.as_deref()),
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

        if let Some(signal) = signal
            && let Some((agent_id, parent_agent_id)) =
                resolve_claude_observation_identity(self.spec().kind, event_name, payload, &hook)
        {
            let mut observation =
                build_claude_observation(payload, &hook, signal, agent_id, parent_agent_id);
            enrich_root_registration(&mut observation, &hook, || {
                oauth_usage::load_account_key(&crate::agents::ambient_env()).ok()
            });
            decoded.attach_lifecycle(observation);
        }
        let final_message = decoded.lifecycle().and_then(|observation| {
            final_message_for_lifecycle(payload, observation, |path| {
                terminal_tail.or_else(|| read_transcript_tail(path))
            })
        });
        decoded.set_final_message(final_message);
        if payload_has_context_observation(payload) {
            decoded.set_observed_context(self.observe_context(self.spec().kind, payload));
        }
        Ok(decoded)
    }

    fn spawned_subagents(&self, input: SubagentSpawnInput<'_>) -> Vec<SpawnedSubagent> {
        input
            .parent_transcript_path
            .map_or_else(Vec::new, subagents::spawned_subagents_under)
    }

    fn ask_options(&self, kind: AskKind) -> Option<Vec<crate::transcript::AskOption>> {
        match kind {
            AskKind::Permission => Some(ask::permission_options()),
            AskKind::PlanApproval => Some(ask::plan_options()),
            AskKind::Question => None,
        }
    }

    fn pane_actions(&self, kind: AskKind) -> Option<&'static str> {
        match kind {
            AskKind::Permission => Some(ask::PERMISSION_PANE_ACTIONS),
            AskKind::PlanApproval => Some(ask::PLAN_PANE_ACTIONS),
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

    fn tool_call_resolved(&self, payload: &Value, native_key: &str) -> bool {
        // Every hook payload carries `transcript_path`, so the proof is one
        // bounded tail read on the event that would otherwise be ignored as a
        // sibling. A missing or unreadable transcript proves nothing.
        transcript_tail_from_payload(payload)
            .is_some_and(|tail| statusline::tool_result_recorded(&tail, native_key))
    }
}

impl crate::agents::capabilities::InstallationCapability for ClaudeAdapter {
    fn folder_trust(
        &self,
        cwd: &Path,
        repo_root: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<crate::agents::FolderTrust> {
        Some(folder_trust::folder_trust(cwd, repo_root, login_env))
    }

    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::SessionCapability for ClaudeAdapter {
    fn discover_local_sessions(
        &self,
        workspaces: &[&Path],
        login_env: &BTreeMap<String, String>,
    ) -> Vec<super::LocalSessionObservation> {
        local_sessions::discover(workspaces, login_env)
    }

    fn local_conversation_present(
        &self,
        session_id: &crate::ids::AgentSessionId,
        cwd: &Path,
        login_env: &BTreeMap<String, String>,
    ) -> Option<bool> {
        local_sessions::conversation_present(session_id, cwd, login_env)
    }
}

impl crate::agents::capabilities::TranscriptCapability for ClaudeAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        statusline::parse_messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for ClaudeAdapter {
    fn prompt_cache_ttl(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(60 * 60))
    }

    fn context_window_for_model(&self, model: &str, prices: &PriceBook) -> Option<u64> {
        context_window_for(model, prices)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        // Claude's transport is the statusline JSON blob. Tolerant parse: any
        // non-object payload yields `None` rather than an error.
        let parsed: statusline::StatuslinePayload = serde_json::from_value(payload.clone()).ok()?;
        let agent_id = parsed.session_id.clone()?;
        let mut context = parsed.into_context(source, Timestamp::now());
        if let Some(tail) = transcript_tail_from_payload(payload) {
            context.turn_error = statusline::detect_turn_error(&tail);
            context.settle = statusline::detect_turn_interrupted(&tail)
                .map(|at| TurnSettle::new(at, TurnSettleOutcome::Interrupted));
        }
        super::ContextObservation::new(agent_id, context)
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

    fn subagent_cost_cursor(
        &self,
        payload: &Value,
        child_id: &str,
        prior: Option<&super::SubagentUsageCursor>,
        prices: &PriceBook,
        book_fingerprint: Option<&str>,
    ) -> Option<super::SubagentUsageCursor> {
        let parent = optional_payload_string(payload, &["transcript_path"])?;
        let prices = managed_pricing::overlay(prices);
        let book_fingerprint = managed_pricing::extend_fingerprint(book_fingerprint);
        subagent_cost::advance_cursor(
            Path::new(&parent),
            child_id,
            prior,
            &prices,
            book_fingerprint.as_deref(),
        )
    }

    fn local_context_refresh(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        if let super::RefreshTrigger::Hook(event_name) = trigger
            && !matches!(
                event_name,
                "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop"
            )
        {
            return None;
        }
        local_context::refresh(ctx)
    }
}

impl crate::agents::capabilities::AccountCapability for ClaudeAdapter {
    fn prepare_reset_credit(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> std::result::Result<crate::agents::account::ResetCreditOffer, String> {
        oauth_usage::prepare_reset_credit(login_env).map_err(|error| error.to_string())
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

    fn remote_control_status(
        &self,
        account: Option<&crate::agents::AgentAccount>,
        login_env: &BTreeMap<String, String>,
    ) -> RemoteControlStatus {
        let (_, settings) = remote_control::read_rc_settings(login_env);
        let version = account
            .and_then(|account| account.version.as_deref())
            .and_then(|version| version.parse().ok());
        RemoteControlStatus {
            pane_auto: remote_control::pane_auto_enabled(&settings, version),
        }
    }
}

impl crate::agents::capabilities::SpendingCapability for ClaudeAdapter {
    fn spending_sources(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<crate::agents::spending::SpendingSource> {
        spend::claude_config_roots(login_env)
            .into_iter()
            .flat_map(|dir| {
                crate::agents::spending::SpendingSource::tree(dir.join("projects"), "**/*.jsonl")
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
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        let matches_session = |path: &Path| {
            path.components()
                .any(|component| component.as_os_str().to_string_lossy().contains(session_id))
        };
        let files: Vec<PathBuf> = self
            .transcript_files(login_env)
            .into_iter()
            .filter(|path| matches_session(path) && !subagents::is_subagent_transcript(path))
            .collect();
        files
            .iter()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("chat.jsonl"))
            .cloned()
            .or_else(|| files.into_iter().next())
    }

    fn session_spend_transcripts(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        let Some(main) = self.session_transcript(session_id, prior_path, login_env) else {
            return Vec::new();
        };
        let mut transcripts = vec![main.clone()];
        transcripts.extend(subagents::subagent_transcripts_under(&main));
        transcripts
    }

    fn spend_pricing_is_current(&self, cursor: &crate::agents::spending::SpendCursor) -> bool {
        cursor.state.as_ref().and_then(Value::as_str)
            == managed_pricing::fingerprint_current().as_deref()
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
        let prices = managed_pricing::overlay(prices);
        let mut parsed =
            spend::parse_claude_spend(path, resume.map_or(0, |cursor| cursor.offset), &prices);
        parsed.cursor.state = managed_pricing::fingerprint_current().map(Value::String);
        parsed
    }
}

impl crate::agents::capabilities::RuntimeControlCapability for ClaudeAdapter {
    fn runtime_control_readiness(
        &self,
        enabled: bool,
        login_env: &BTreeMap<String, String>,
    ) -> super::runtime_control::RuntimeControlReadiness {
        remote_control::readiness(enabled, login_env)
    }

    fn prepare_runtime_control(&self, enabled: bool, login_env: &BTreeMap<String, String>) {
        remote_control::ensure_consent(enabled, login_env);
    }

    fn runtime_control_liveness(
        &self,
        project_root: &Path,
        login_env: &BTreeMap<String, String>,
    ) -> super::runtime_control::RuntimeControlLiveness {
        remote_liveness::probe(project_root, login_env)
    }

    fn runtime_control_wiring_input_path(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        Some(remote_control::settings_path(login_env))
    }
}

/// The one typed payload a Claude hook carries, parsed once per event.
enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
    UserPromptSubmit(ClaudeUserPromptSubmit),
    SubagentStart(ClaudeSubagentStart),
    SubagentStop(ClaudeSubagentStop),
    Stop(ClaudeStop),
    StopFailure(ClaudeStopFailure),
    PreToolUse(ClaudePreToolUse),
    PostToolUse(ClaudePostToolUse),
    PermissionRequest(ClaudePermissionRequest),
    PreCompact,
    PostCompact(ClaudePostCompact),
    SessionEnd,
    Other,
}

impl ClaudeHook {
    fn parse(event_name: &str, payload: &Value) -> Self {
        match event_name {
            "SessionStart" => Self::SessionStart(parse(payload)),
            "UserPromptSubmit" => Self::UserPromptSubmit(parse(payload)),
            "SubagentStart" => Self::SubagentStart(parse(payload)),
            "SubagentStop" => Self::SubagentStop(parse(payload)),
            "Stop" => Self::Stop(parse(payload)),
            "StopFailure" => Self::StopFailure(parse(payload)),
            "PreToolUse" => Self::PreToolUse(parse(payload)),
            "PostToolUse" => Self::PostToolUse(parse(payload)),
            "PermissionRequest" => Self::PermissionRequest(parse(payload)),
            "PreCompact" => Self::PreCompact,
            "PostCompact" => Self::PostCompact(parse(payload)),
            "SessionEnd" => Self::SessionEnd,
            _ => Self::Other,
        }
    }

    fn subagent_common(&self) -> Option<&ClaudeCommon> {
        match self {
            Self::SubagentStart(p) => Some(&p.common),
            Self::SubagentStop(p) => Some(&p.common),
            _ => None,
        }
    }
}

fn map_claude_lifecycle_signal(
    spec: &AgentSpec,
    payload: &Value,
    hook: &ClaudeHook,
) -> Option<LifecycleSignal> {
    // Claude stamps the same `tool_use_id` on a call's `PreToolUse` and
    // `PostToolUse`, and a distinct one per parallel call, so it is the native
    // key that keeps an open ask alive across a sibling tool's edge. A build
    // that omits it degrades to the keyless behaviour. `PermissionRequest` has
    // no id on the wire and stays keyless on purpose: its clearing edge is the
    // approved tool's keyed `PostToolUse`, which the sibling guard admits
    // because the guard needs the *open ask* to be keyed.
    let tool_use_id = optional_payload_string(payload, &["tool_use_id"]);
    match hook {
        ClaudeHook::SessionStart(start) => Some(start.source.session_start_signal()),
        ClaudeHook::UserPromptSubmit(_) => Some(LifecycleSignal::TurnStarted { turn_id: None }),
        ClaudeHook::SubagentStart(_) => Some(LifecycleSignal::SubagentStarted),
        // The published SubagentStop payload has no outcome or exit-code
        // field, so close the bracket without inventing an error state.
        ClaudeHook::SubagentStop(_) => Some(LifecycleSignal::SubagentStopped { errored: false }),
        ClaudeHook::Stop(stop) => Some(LifecycleSignal::TurnEnded {
            errored: stop_payload_errored(payload),
            parked_on_background: has_pending_background(
                stop.background_tasks.as_deref().unwrap_or_default(),
                &stop.session_crons,
            ),
            turn_id: None,
        }),
        ClaudeHook::PermissionRequest(request) => spec
            .blocking_tool_kind(request.tool_name.as_deref())
            .is_none()
            .then_some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            }),
        ClaudeHook::PostToolUse(tool) => Some(LifecycleSignal::ToolUsed {
            mutates: spec.tool_mutates(payload),
            edits: spec.tool_edits_files(payload),
            name: tool.tool_name.clone(),
            native_key: tool_use_id,
            turn_id: None,
        }),
        ClaudeHook::PreToolUse(request) => {
            match spec.blocking_tool_kind(request.tool_name.as_deref()) {
                Some(kind) => Some(LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                    native_key: tool_use_id,
                }),
                None => Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    name: None,
                    native_key: tool_use_id,
                    turn_id: None,
                }),
            }
        }
        ClaudeHook::PreCompact => Some(LifecycleSignal::Compacting),
        ClaudeHook::PostCompact(compact) => Some(LifecycleSignal::CompactionEnded {
            auto: compact.trigger.auto_flag(),
            failed: false,
        }),
        ClaudeHook::SessionEnd => Some(LifecycleSignal::Ended),
        ClaudeHook::StopFailure(_) | ClaudeHook::Other => None,
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
    hook: &ClaudeHook,
) -> Option<ObservationIdentity> {
    let typed_common = hook.subagent_common();
    let payload_agent_id = optional_payload_string(payload, &["agent_id"]);
    let payload_session_id = optional_payload_string(payload, &["session_id"]);
    let child_id = typed_common
        .and_then(|common| common.agent_id.as_deref())
        .or(payload_agent_id.as_deref());
    let parent_id = typed_common
        .and_then(|common| common.common.session_id.as_deref())
        .or(payload_session_id.as_deref());
    let distinct_child = child_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .zip(parent_id.map(str::trim).filter(|value| !value.is_empty()))
        .is_some_and(|(child, parent)| child != parent);

    if typed_common.is_some() || distinct_child {
        match resolve_subagent_identity(kind, event_name, child_id, parent_id, payload) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => Some((Some(agent_id), Some(parent_agent_id))),
            SubagentIdentity::Quarantined => None,
        }
    } else {
        match resolve_root_identity(kind, event_name, child_id, parent_id) {
            RootIdentity::Root { agent_id } => Some((agent_id, None)),
            RootIdentity::ForeignChild => None,
        }
    }
}

fn enrich_root_registration(
    observation: &mut AgentLifecycleObservation,
    hook: &ClaudeHook,
    load_account_key: impl FnOnce() -> Option<String>,
) {
    if observation.parent_agent_id.is_some()
        || !matches!(observation.signal, LifecycleSignal::Registered)
    {
        return;
    }
    observation.account_key = load_account_key();
    let ClaudeHook::SessionStart(start) = hook else {
        return;
    };
    observation.origin = match start.source {
        SessionSource::Startup | SessionSource::Clear => Some(SessionOrigin::Fresh),
        SessionSource::Fork => Some(SessionOrigin::Forked),
        SessionSource::Resume | SessionSource::Compact | SessionSource::Unknown => None,
    };
}

fn build_claude_observation(
    payload: &Value,
    hook: &ClaudeHook,
    signal: LifecycleSignal,
    agent_id: Option<crate::ids::AgentSessionId>,
    parent_agent_id: Option<crate::ids::AgentSessionId>,
) -> AgentLifecycleObservation {
    let transcript_path = optional_payload_string(payload, &["session_id"])
        .and_then(|_| optional_payload_string(payload, &["transcript_path"]));
    // A subagent payload carries both transcripts: `transcript_path` is the
    // parent's and `agent_transcript_path` is the child's. Reading the parent's
    // for a child would stamp the parent's newest model and token total onto the
    // child's row, so a child sources usage from its own transcript alone —
    // absent that, usage stays unknown rather than borrowed.
    let usage_path = match hook {
        ClaudeHook::SubagentStop(stop) => stop.agent_transcript_path.clone(),
        _ if parent_agent_id.is_some() => None,
        _ => transcript_path.clone(),
    };
    let usage = usage_path
        .as_deref()
        .map(Path::new)
        .map(usage_from_transcript)
        .unwrap_or_default();
    let payload_model = match hook {
        ClaudeHook::SessionStart(start) => start.common.model.clone(),
        _ => None,
    }
    .or_else(|| optional_payload_string(payload, &["model"]));
    let model = payload_model.clone().or(usage.model);
    // Assert a window only when the `[1m]` marker is actually present; a
    // marker-less hook leaves it `None` so the established window carries
    // forward. The gauge percentage is derived downstream from the folded
    // window, never baked here against a guessed denominator.
    let context_window = extended_context_window(model.as_deref());
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    // Shells belong to the root session: a subagent's hooks target its child
    // row, and the parent's `Stop` lists every shell the session runs.
    if parent_agent_id.is_none() {
        observation.background_shells = claude_background_shells(hook, Timestamp::now());
    }
    observation.parent_agent_id = parent_agent_id;
    observation.task = claude_task(payload, hook.subagent_common());
    observation.prompt = SanitizedPrompt::new(match hook {
        ClaudeHook::UserPromptSubmit(submit) => submit.prompt.as_deref(),
        _ => None,
    });
    observation.transcript_path = transcript_path;
    observation.launch.model = model;
    observation.launch.effort = claude_effort(payload, hook);
    observation.usage.context_window = context_window;
    // A child's transcript total covers its last request only, not its run, so
    // a child carries the request split and no total.
    if observation.parent_agent_id.is_none() {
        observation.usage.total_tokens = payload_total_tokens(payload, usage.total_tokens);
    }
    observation.usage.fresh_input_tokens = usage.fresh_input_tokens;
    observation.usage.cache_read_input_tokens = usage.cache_read_input_tokens;
    observation.usage.cache_write_input_tokens = usage.cache_write_input_tokens;
    observation.usage.output_tokens = usage.output_tokens;
    observation
}

fn final_message_for_lifecycle(
    payload: &Value,
    observation: &AgentLifecycleObservation,
    read_tail: impl FnOnce(&Path) -> Option<String>,
) -> Option<String> {
    let needs_terminal_message = observation.signal.terminal_disposition().is_some();
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

fn claude_effort(payload: &Value, hook: &ClaudeHook) -> Option<String> {
    match hook {
        ClaudeHook::Stop(stop) => stop.common.effort.as_ref(),
        ClaudeHook::SubagentStop(stop) => stop.common.effort.as_ref(),
        _ => None,
    }
    .and_then(|e| e.level.clone())
    .or_else(|| optional_payload_string(payload, &["thinking_level"]))
}

/// What a root hook proves about the session's background shells: a Bash
/// launch that returned a `backgroundTaskId` (explicit `run_in_background` or
/// an auto-backgrounded timeout), a `Stop` task list, or a task-notification
/// prompt for tasks that stopped running.
fn claude_background_shells(hook: &ClaudeHook, now: Timestamp) -> Option<BackgroundShellReport> {
    match hook {
        ClaudeHook::PostToolUse(post) => {
            if post.tool_name.as_deref() != Some("Bash") {
                return None;
            }
            let id = post
                .tool_response
                .as_ref()?
                .get("backgroundTaskId")?
                .as_str()
                .filter(|id| !id.is_empty())?;
            let input_text = |key: &str| {
                post.tool_input
                    .as_ref()
                    .and_then(|input| input.get(key))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            };
            Some(BackgroundShellReport::Started {
                shell: BackgroundShell {
                    id: id.to_owned(),
                    command: input_text("command"),
                    description: input_text("description"),
                    started_at: now,
                },
            })
        }
        ClaudeHook::Stop(stop) => {
            let shells = stop
                .background_tasks
                .as_deref()?
                .iter()
                .filter(|task| task.is_pending())
                .filter_map(|task| task.as_shell(now))
                .collect();
            Some(BackgroundShellReport::Snapshot { shells })
        }
        ClaudeHook::UserPromptSubmit(submit) => {
            let ids = finished_task_notification_ids(submit.prompt.as_deref()?);
            (!ids.is_empty()).then_some(BackgroundShellReport::Finished { ids })
        }
        _ => None,
    }
}

/// Claude v2.1.145+ parks on nonterminal background tasks or any scheduled wakeup.
///
/// Older builds omit both arrays and genuinely end the turn.
fn has_pending_background(tasks: &[BackgroundTask], crons: &[payloads::SessionCron]) -> bool {
    tasks.iter().any(BackgroundTask::is_pending) || !crons.is_empty()
}

/// Context-window usage derived from a Claude transcript tail. Carries the
/// latest turn's token total (context-occupying input plus output), the gauge
/// numerator the fold scales against the resolved window, plus the four
/// components that total is summed from. Claude reports the split on every
/// assistant record, so keeping it costs nothing and lets the card show where
/// the window actually went (fresh input vs. cache reuse vs. output).
#[derive(Default)]
struct TranscriptUsage {
    total_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    model: Option<String>,
}

impl TranscriptUsage {
    /// A transcript that opened cleanly but carries no assistant usage yet — a
    /// brand-new session. Report an explicit zero so the gauge draws an empty
    /// bar at 0% instead of vanishing until the first turn completes. A
    /// transcript that cannot be read stays `default()` (all `None`): unknown,
    /// not zero.
    ///
    /// Only the total is zeroed. The breakdown stays `None` so a tail that
    /// simply outran its last usage record reports "unknown" for the split
    /// rather than asserting four confident zeroes.
    fn fresh() -> Self {
        Self {
            total_tokens: Some(0),
            ..Self::default()
        }
    }
}

/// The 1M-token context window when the model id carries the `[1m]` beta marker
/// (`claude-opus-4-8[1m]`), else `None` — a bare id is *unknown*, not 200k. The
/// marker rides only the hook payload's `model` field (the transcript always
/// writes the bare id), so a marker-less hook cannot distinguish a true 200k
/// model from a 1M model whose payload dropped the marker. Returning `None`
/// keeps the last established window (and the 200k spec default applies
/// when none was ever seen), so the gauge never downgrades a 1M agent to 200k.
fn extended_context_window(model: Option<&str>) -> Option<u64> {
    const EXTENDED: u64 = 1_000_000;
    model
        .filter(|model| model.contains("[1m]"))
        .map(|_| EXTENDED)
}

fn context_window_for(model: &str, prices: &PriceBook) -> Option<u64> {
    extended_context_window(Some(model)).or_else(|| {
        prices
            .exact_price(model)
            .and_then(|price| price.max_input_tokens)
    })
}

/// Derive context-window usage from the tail of a Claude transcript JSONL.
/// Claude never puts token counts in the hook payload — they live in the
/// transcript — so this is the only place the context gauge can be sourced.
/// Reads a bounded tail and takes the most recent assistant `message.usage`,
/// or the newer `compact_boundary` record's post-compaction size.
/// Best-effort: any IO or parse failure yields empty fields (enrichment, never
/// correctness).
fn usage_from_transcript(path: &Path) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(path) else {
        return TranscriptUsage::default();
    };
    usage_from_transcript_tail(&text, None)
}

fn usage_from_transcript_tail(text: &str, agent_id: Option<&str>) -> TranscriptUsage {
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
        if agent_id
            .is_some_and(|agent_id| value.get("agentId").and_then(Value::as_str) != Some(agent_id))
        {
            continue;
        }
        // Every usage record older than a compaction boundary was measured
        // before the compaction, so the boundary ends the walk with the size
        // it records for the compacted window, or with no reading.
        if value.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
            return TranscriptUsage {
                total_tokens: value
                    .pointer("/compactMetadata/postTokens")
                    .and_then(Value::as_u64),
                ..TranscriptUsage::default()
            };
        }
        let message = value.get("message");
        let Some(usage) = message.and_then(|m| m.get("usage")) else {
            continue;
        };
        let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let fresh_input = field("input_tokens");
        let cache_read = field("cache_read_input_tokens");
        let cache_write = field("cache_creation_input_tokens");
        let context_tokens = fresh_input + cache_read + cache_write;
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
            fresh_input_tokens: Some(fresh_input),
            cache_read_input_tokens: Some(cache_read),
            cache_write_input_tokens: Some(cache_write),
            output_tokens: Some(output),
            model,
        };
    }
    TranscriptUsage::fresh()
}

#[cfg(test)]
mod tests;
