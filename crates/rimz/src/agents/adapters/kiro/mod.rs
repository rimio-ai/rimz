//! Kiro CLI v3 native-hook, local-session, transcript, and live-state adapter.
//!
//! Since Kiro CLI 2.13.0 the global `~/.kiro/hooks/` files fire in every
//! workspace, so a managed hook file reports session, turn, and tool
//! lifecycle. The stock structured session store still supplies the newborn
//! card before the first prompt, approval waits, cancel and failure outcomes,
//! context percentage, and history.

mod install;
mod session;
// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::AccountCapability for KiroAdapter {}
impl crate::agents::capabilities::RuntimeControlCapability for KiroAdapter {}

#[cfg(test)]
mod tests;

use crate::agents::capabilities::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RemoteControlCapability, ThreadKey,
    ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::{
    AgentLifecycleObservation, HookOutput, HookRouting, LocalContextPatch, LocalContextRefresh,
    LocalContextRefreshCtx, LocalSessionObservation, RefreshTrigger, Result, SanitizedPrompt,
    TranscriptMessage, TranscriptStat, optional_payload_string, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;
use serde_json::Value;

static KIRO_DESCRIPTOR: AgentSpec = AgentSpec {
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    kind: "kiro",
    aliases: &[],
    bin_names: &["kiro-cli"],
    bin_identity: None,
    display_name: "Kiro",
    brand: Brand {
        emblem: None,
        color: 92,
        color_rgb: (0x79, 0x0e, 0xcb),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Kiro" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &["fs_write"],
        editing: &["fs_write"],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: false,
        // Kiro can draw native prompts, but v3 exposes no hook that records
        // them for RimZ routing.
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: KIRO_COVERAGE,
    user_coverage: KIRO_USER_COVERAGE,
    lifecycle_hooks: KIRO_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    // `kiro-cli` launches the session; the v3 chat engine lives in the separate
    // `kiro-cli-chat` binary the launcher execs into, so a live pane can read as
    // either. `kiro-cli-term` is the figterm shell-integration daemon (it runs
    // for every integrated shell, not just agent panes), so it is deliberately
    // excluded to avoid false-positive presence.
    process_names: &["kiro-cli", "kiro-cli-chat"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        definitions: crate::agents::definition::DefinitionSpec::EMPTY,
        program: Some("kiro-cli"),
        fixed_args: &["chat", "--v3"],
        // The v3 TUI re-parses argv and treats `--` as an unknown flag that
        // swallows the next token, so the prompt must stay a bare positional.
        prompt: super::PromptStyle::Positional,
        resume: Some(super::SessionCommand {
            before_id: &["kiro-cli", "chat", "--v3", "--resume-id"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        interrupt_key: None,
        compact_command: Some(super::CompactCommand {
            command: "/compact",
            instruction: super::CompactInstruction::Unsupported,
        }),
        presets: super::PresetMatchers {
            auto_compact: None,
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: None,
        },
    },
};

const KIRO_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Partial {
        via: "unresolved pending_interaction tool_approval records",
        gap: "waiting is visible but not routable through rimz asks/answer",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no v3 plan-approval hook",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "no v3 native-question hook",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native prompt choreography is not mapped",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "no compaction hook; /compact leaves only a tombstone record afterwards",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "default subagents skip hooks and publish no child lifecycle",
    },
    launch_reminders: ConcernCoverage::Unsupported {
        reason: "no additive system-text launch channel is implemented",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    background_shells: ConcernCoverage::Unsupported {
        reason: "background shells are not mapped",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "no SessionEnd hook; v3 Stop is turn end, not session end",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "Stop hook plus session_pause and turn_end records",
        gap: "no native Notification event",
    },
    context_usage: ConcernCoverage::Partial {
        via: "latest contextUsage session_metadata percentage",
        gap: "percentage only; no token counts or context-window size",
    },
    realtime_cost: ConcernCoverage::Unsupported {
        reason: "credit-metered; no machine-readable usage surface",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "hooks publish no model, effort, context, or transcript contract",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.kiro/hooks/rimz.json",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "whoami schema and credit ledger are unpublished",
    },
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no stock-TUI remote-control surface",
    },
};

const KIRO_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Partial {
        shows: "the card follows every turn and tool call as Kiro reports it",
        limit: "cancels and failures show only once Kiro's session store records them",
    },
    live: CapabilityLevel::Partial {
        shows: "a context-fill percentage",
        limit: "no token counts, no context-window size, and no dollar figure",
    },
    history: CapabilityLevel::Partial {
        shows: "past sessions read end to end from Kiro's own store",
        limit: "no tokens or dollars, so Kiro stays out of rimz stats",
    },
    account: CapabilityLevel::Unsupported {
        reason: "Kiro publishes no readable login, plan, or credit ledger",
    },
    ask: CapabilityLevel::Partial {
        shows: "a pending tool approval raises Waiting and routes you to the pane",
        limit: "the prompt stays in Kiro's own UI, so rimz asks stays empty",
    },
    subagents: CapabilityLevel::Unsupported {
        reason: "Kiro publishes no child lifecycle",
    },
};

const KIRO_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
    awaiting_input: HookCoverage::Derived {
        via: "unresolved pending_interaction tool approval",
        gap: "native prompt is visible but has no structured RimZ answer route",
    },
    subagent_started: HookCoverage::Absent {
        reason: "default subagents skip hooks",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "default subagents skip hooks",
    },
    compacting: HookCoverage::Absent {
        reason: "no compaction hook",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "no compaction hook",
    },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "Stop ends a turn, not the session",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const KIRO_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "SessionStart",
        r#"{"session_id":"sess_redacted","hook_event_name":"SessionStart"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"session_id":"sess_redacted","hook_event_name":"UserPromptSubmit","prompt":"ping"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PostToolUse",
        r#"{"session_id":"sess_redacted","hook_event_name":"PostToolUse","tool_name":"fs_write"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "Stop",
        r#"{"session_id":"sess_redacted","hook_event_name":"Stop"}"#,
    )
    .progress(),
];

/// Carries [`super::managed_source::RIMZ_MANAGED_MARKER`] on its first line;
/// the v3 hook schema accepts the extra top-level key. Each command swallows a
/// feed failure because a `Stop` hook exiting 1 continues the turn.
const HOOK_SOURCE: &str = include_str!("hooks.json");

const KIRO_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "kiro",
    HOOK_SOURCE,
    KIRO_HOOKS,
    "hook file",
    install::hooks_path,
    true,
);

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct KiroAdapter;

impl crate::agents::capabilities::CoreCapability for KiroAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &KIRO_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            classification: super::hook_types::catalog_classification_corpus(KIRO_HOOKS),
            local_session: Some(session::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::LaunchCapability for KiroAdapter {
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["KIRO_HOME"]
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        install::resolve_home(
            env.get("KIRO_HOME").map(std::ffi::OsStr::new),
            env.get("HOME").map(std::ffi::OsStr::new),
        )
    }

    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        Some(self.config_home(env)?.join("skills"))
    }
}

impl crate::agents::capabilities::HookCapability for KiroAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let mut decoded = decode_catalog_hook(KIRO_HOOKS, event_name, None);
        let agent_id = optional_payload_string(payload, &["session_id"]).map(AgentSessionId::from);
        decoded.set_routing(HookRouting::session(agent_id.clone()));
        let signal = match event_name {
            // Kiro's payload carries no start source.
            "SessionStart" => SessionSource::Startup.session_start_signal(),
            "UserPromptSubmit" => LifecycleSignal::TurnStarted { turn_id: None },
            "PostToolUse" => LifecycleSignal::ToolUsed {
                mutates: self.spec().tool_mutates(payload),
                edits: self.spec().tool_edits_files(payload),
                name: None,
                native_key: None,
                turn_id: None,
            },
            // Cancelled and failed turns settle from the session store.
            "Stop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            _ => return Ok(decoded),
        };
        let mut observation = AgentLifecycleObservation::new(agent_id.clone(), signal)
            .with_worktree_from_payload(payload);
        if event_name == "UserPromptSubmit" {
            let prompt = optional_payload_string(payload, &["prompt"]);
            observation.task = sanitize_user_prompt(prompt.as_deref());
            observation.prompt = SanitizedPrompt::new(prompt.as_deref());
        }
        if event_name == "Stop" {
            decoded.set_final_message(
                agent_id
                    .as_ref()
                    .and_then(|id| session::last_reply(id.as_str(), &super::ambient_env())),
            );
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for KiroAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

impl crate::agents::capabilities::SessionCapability for KiroAdapter {
    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }

    fn discover_local_sessions(
        &self,
        workspaces: &[&Path],
        login_env: &BTreeMap<String, String>,
    ) -> Vec<LocalSessionObservation> {
        session::discover(workspaces, login_env)
    }
}

impl crate::agents::capabilities::TranscriptCapability for KiroAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for KiroAdapter {
    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let path = self.session_transcript(
            ctx.agent_id,
            ctx.prior_transcript_path.map(Path::new),
            ctx.login_env,
        )?;
        let stat = ctx.changed_transcript(TranscriptStat::from_path(&path)?)?;
        Some(LocalContextRefresh {
            context: LocalContextPatch::authoritative_current(),
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
            ..LocalContextRefresh::authoritative_current()
        })
    }
}

impl crate::agents::capabilities::SpendingCapability for KiroAdapter {
    fn session_transcript(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| session::valid_transcript(path, session_id)) {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session(session_id, login_env)
    }
}
