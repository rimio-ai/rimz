//! Amp plugin adapter.
//!
//! Amp has no command-hook or statusline protocol. RimZ installs a small
//! observation-only TypeScript plugin that forwards the active thread's native
//! lifecycle events without entering Amp's tool-decision path.

mod account;
mod payloads;

use crate::agents::capabilities::*;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, LocalSessionDiscovery, PlanLabel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::{
    AgentErr, AgentLifecycleObservation, AskKind, HookOutput, HookRouting, Result, SanitizedPrompt,
    SessionOrigin, non_empty_trimmed, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static AMP_DESCRIPTOR: AgentSpec = AgentSpec {
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    kind: "amp",
    aliases: &[],
    display_name: "Amp",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xee, 0xee, 0xee),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &["shell_command", "apply_patch", "create_file", "edit_file"],
        editing: &["apply_patch", "create_file", "edit_file"],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: LocalSessionDiscovery::Unsupported,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: AMP_COVERAGE,
    user_coverage: AMP_USER_COVERAGE,
    lifecycle_hooks: AMP_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["amp", "node"],
    bin_names: &["amp"],
    bin_identity: None,
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        definitions: crate::agents::definition::DefinitionSpec::EMPTY,
        program: Some("amp"),
        fixed_args: &[],
        prompt: super::PromptStyle::FlagWithSuffix {
            flag: "-x",
            // Amp refuses to continue an archived thread, and execute mode
            // archives by default, so supervised threads stay unarchived.
            suffix: &["--plugin-ready-timeout", "30", "--no-archive-after-execute"],
        },
        resume: Some(super::SessionCommand {
            before_id: &["amp", "threads", "continue"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        interrupt_key: None,
        compact_command: None,
        presets: super::PresetMatchers {
            auto_compact: None,
            model: Some(super::StaticPresetMatcher::Flag(&["--mode"])),
            effort: None,
            system_prompt_file: None,
            fast: super::FastSwitch::Args(&["--fast"]),
        },
        ..super::LaunchSpec::EMPTY
    },
};

const AMP_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "agent_start/agent_end",
    },
    permission: ConcernCoverage::Wired {
        via: "thread-state awaiting-approval",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no native event",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "Amp's ask_user_choice question waits are not observable",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no external resolver",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "automatic compaction has no event",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "interactive events expose no durable child identity",
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
        gap: "no session-end event",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + awaiting-approval + stall window",
        gap: "no notification event",
    },
    context_usage: ConcernCoverage::Unsupported {
        reason: "Amp no longer writes a local thread cache",
    },
    realtime_cost: ConcernCoverage::Unsupported {
        reason: "Amp no longer writes a local thread cache",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "no out-of-band context transport",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.config/amp/plugins/rimz.ts",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "Amp no longer writes a local thread cache",
    },
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "readiness is not detectable",
    },
};

const AMP_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card follows every turn and clears when the pane goes away",
    },
    live: CapabilityLevel::Unsupported {
        reason: "Amp keeps no local token or cost record",
    },
    history: CapabilityLevel::Unsupported {
        reason: "Amp keeps no local record of past thread spend",
    },
    account: CapabilityLevel::Partial {
        shows: "whether an Amp login is present on this machine",
        limit: "no identity, plan, credits, or usage window",
    },
    ask: CapabilityLevel::Partial {
        shows: "a thread waiting on tool approval raises Waiting",
        limit: "Amp's default asks no approvals and ask_user_choice questions go unseen",
    },
    subagents: CapabilityLevel::Unsupported {
        reason: "Amp's events expose no durable child identity",
    },
};

const AMP_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Native {
        event: "session_start",
    },
    turn_started: HookCoverage::Native {
        event: "agent_start",
    },
    turn_ended: HookCoverage::Native { event: "agent_end" },
    tool_used: HookCoverage::Native {
        event: "tool_result",
    },
    awaiting_input: HookCoverage::Native {
        event: "permission_ask",
    },
    subagent_started: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    compacting: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const AMP_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "session_start",
        r#"{"session_id":"T-abc123","cwd":"/tmp/repo"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "agent_start",
        r#"{"session_id":"T-abc123","prompt":"fix auth"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "tool_result",
        r#"{"session_id":"T-abc123","tool_name":"apply_patch","status":"done","files_modified":true}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "agent_end",
        r#"{"session_id":"T-abc123","status":"done"}"#
    )
    .progress(),
    HookEventSpec::blocking(
        "permission_ask",
        r#"{"session_id":"T-abc123"}"#,
        AskKind::Permission
    )
    .synchronous(),
];
const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const AMP_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "amp",
    PLUGIN_SOURCE,
    AMP_HOOKS,
    "plugin",
    amp_plugin_path,
    false,
);

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct AmpAdapter;

impl crate::agents::capabilities::CoreCapability for AmpAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &AMP_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            classification: super::hook_types::catalog_classification_corpus(AMP_HOOKS),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for AmpAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let ask_kind = (event_name == "permission_ask").then_some(AskKind::Permission);
        let mut decoded = decode_catalog_hook(AMP_HOOKS, event_name, ask_kind);
        let parsed = payloads::parse_payload(payload);
        let session_id = parsed
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty());
        decoded.set_routing(HookRouting::session(session_id.map(Into::into)));
        decoded.set_final_message(
            (event_name == "agent_end")
                .then_some(parsed.last_assistant_message.as_deref())
                .flatten()
                .and_then(non_empty_trimmed),
        );

        let Some(session_id) = session_id else {
            return Ok(decoded);
        };
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            "agent_start" => LifecycleSignal::TurnStarted { turn_id: None },
            "tool_result" => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: parsed
                    .files_modified
                    .unwrap_or_else(|| self.spec().tool_edits_files(payload)),
                name: None,
                native_key: None,
                turn_id: None,
            },
            "agent_end" => match parsed.status.as_deref() {
                Some("cancelled") => LifecycleSignal::TurnInterrupted { turn_id: None },
                status => LifecycleSignal::TurnEnded {
                    errored: status != Some("done"),
                    parked_on_background: false,
                    turn_id: None,
                },
            },
            "permission_ask" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            _ => return Ok(decoded),
        };
        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from(session_id)), signal)
                .with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = SanitizedPrompt::new(parsed.prompt.as_deref());
        observation.launch.model = parsed.model;
        observation.launch.effort = parsed.effort;
        if event_name == "session_start" {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for AmpAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&AMP_MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::LaunchCapability for AmpAdapter {
    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        amp_config_home(
            env.get("XDG_CONFIG_HOME").map(std::ffi::OsStr::new),
            env.get("HOME").map(std::ffi::OsStr::new),
        )
    }

    fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        parse_amp_version(stdout).or_else(|| parse_amp_version(stderr))
    }
}

impl crate::agents::capabilities::AccountCapability for AmpAdapter {
    fn probe_account(
        &self,
        _login_env: &BTreeMap<String, String>,
    ) -> crate::agents::account::AccountProbe {
        account::probe()
    }
}

fn parse_amp_version(output: &str) -> Option<String> {
    let line = output.lines().find(|line| !line.trim().is_empty())?.trim();
    let (token, annotation) = line.split_once(' ')?;
    if !annotation.starts_with("(released ") || !annotation.ends_with(')') {
        return None;
    }
    let (base, hash) = token.split_once("-g")?;
    let base = base.parse::<super::version::CliVersion>().ok()?;
    (!hash.is_empty() && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
        .then(|| format!("{base}-g{hash}"))
}

fn amp_plugin_path(_login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("RIMZ_AMP_PLUGIN").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let config_home = amp_config_home(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| AgentErr::Install {
        agent: "amp",
        reason: "$HOME is not set; cannot resolve ~/.config/amp/plugins/rimz.ts".to_owned(),
    })?;
    Ok(config_home.join("plugins/rimz.ts"))
}

fn amp_config_home(
    configured: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .map(|home| home.join("amp"))
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for AmpAdapter {}
impl crate::agents::capabilities::SessionCapability for AmpAdapter {}
impl crate::agents::capabilities::TranscriptCapability for AmpAdapter {}
impl crate::agents::capabilities::ContextCapability for AmpAdapter {}
impl crate::agents::capabilities::SpendingCapability for AmpAdapter {}

#[cfg(test)]
mod tests;
