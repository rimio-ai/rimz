//! Launch-only TRAE CLI adapter. Native CLI hook, session, transcript, and
//! usage contracts remain unverified; no IDE protocol is reused here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agents::capabilities::*;
use crate::agents::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, CompactCommand, CompactInstruction,
    ConcernCoverage, CoverageAnnotations, HookCoverage, LaunchPermissionArgs, LaunchSpec,
    LifecycleAnnotations, PlanLabel, PresetMatchers, PromptStyle, RemoteControlCapability,
    StaticPresetMatcher, ThreadKey, ToolClassification, UserCoverage,
};

#[cfg(test)]
mod tests;

const CLI_HOOK_GAP: ConcernCoverage = ConcernCoverage::Unsupported {
    reason: "TRAE CLI hook names and payloads are unverified; IDE hooks do not apply",
};
const SESSION_GAP: ConcernCoverage = ConcernCoverage::Unsupported {
    reason: "TRAE CLI session and transcript storage contracts are unverified",
};
const USAGE_GAP: ConcernCoverage = ConcernCoverage::Unsupported {
    reason: "TRAE CLI usage and account probe schemas are unverified",
};
const NO_SIGNAL: HookCoverage = HookCoverage::Absent {
    reason: "TRAE CLI lifecycle payloads are unverified",
};

static TRAE_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "trae",
    aliases: &[],
    display_name: "Trae",
    brand: Brand {
        emblem: None,
        color: 42,
        color_rgb: (0x32, 0xd5, 0x83),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Trae" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &[],
        editing: &[],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: false,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: crate::agents::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: CoverageAnnotations {
        turn_lifecycle: CLI_HOOK_GAP,
        permission: CLI_HOOK_GAP,
        plan_approval: CLI_HOOK_GAP,
        user_question: CLI_HOOK_GAP,
        answer: CLI_HOOK_GAP,
        compaction: CLI_HOOK_GAP,
        subagents: CLI_HOOK_GAP,
        launch_reminders: ConcernCoverage::Unsupported {
            reason: "no verified additive system-text launch channel",
        },
        background_parking: CLI_HOOK_GAP,
        background_shells: CLI_HOOK_GAP,
        session_end: CLI_HOOK_GAP,
        idle_notification: CLI_HOOK_GAP,
        context_usage: USAGE_GAP,
        realtime_cost: USAGE_GAP,
        rich_context: SESSION_GAP,
        hook_install: CLI_HOOK_GAP,
        account_spend: USAGE_GAP,
        tool_stats: SESSION_GAP,
        remote_control: ConcernCoverage::Unsupported {
            reason: "TRAE CLI ACP handshake and remote-control contracts are unverified",
        },
    },
    user_coverage: UserCoverage {
        state: CapabilityLevel::Unsupported {
            reason: "presence only; turn and wait signals remain unverified",
        },
        live: CapabilityLevel::Unsupported {
            reason: "context and usage payloads have no verified CLI contract",
        },
        history: CapabilityLevel::Unsupported {
            reason: "session storage and transcript schemas remain unverified",
        },
        account: CapabilityLevel::Unsupported {
            reason: "login and account usage probe schemas remain unverified",
        },
        ask: CapabilityLevel::Unsupported {
            reason: "CLI approval and question payloads remain unverified",
        },
        subagents: CapabilityLevel::Unsupported {
            reason: "CLI delegation lifecycle and child identity remain unverified",
        },
    },
    lifecycle_hooks: LifecycleAnnotations {
        registered: NO_SIGNAL,
        turn_started: NO_SIGNAL,
        turn_ended: NO_SIGNAL,
        tool_used: NO_SIGNAL,
        awaiting_input: NO_SIGNAL,
        subagent_started: NO_SIGNAL,
        subagent_stopped: NO_SIGNAL,
        compacting: NO_SIGNAL,
        compaction_ended: NO_SIGNAL,
        ended: NO_SIGNAL,
        lost: HookCoverage::Derived {
            via: "rimz reaper",
            gap: "native CLI hooks do not report mux-session death",
        },
    },
    default_context_window: None,
    default_model: None,
    process_names: &["traecli"],
    bin_names: &["traecli"],
    bin_identity: None,
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    launch: LaunchSpec {
        program: Some("traecli"),
        prompt: PromptStyle::Positional,
        picker: Some(&["traecli", "resume"]),
        permission: LaunchPermissionArgs {
            yolo: &["--yolo"],
            ..LaunchPermissionArgs::EMPTY
        },
        compact_command: Some(CompactCommand {
            command: "/compact",
            instruction: CompactInstruction::Unsupported,
        }),
        presets: PresetMatchers {
            model: Some(StaticPresetMatcher::Flag(&["--model"])),
            ..PresetMatchers::EMPTY
        },
        ..LaunchSpec::EMPTY
    },
};

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct TraeAdapter;

impl CoreCapability for TraeAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &TRAE_DESCRIPTOR
    }
}

impl LaunchCapability for TraeAdapter {
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["TRAE_HOME"]
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        let home = env
            .get("HOME")
            .filter(|value| !value.is_empty())
            .map(std::path::Path::new)
            .map(crate::utils::path::normalize_path_lexical);
        env.get("TRAE_HOME")
            .filter(|value| !value.is_empty())
            .map(std::path::Path::new)
            .filter(|path| path.is_absolute())
            .map(crate::utils::path::normalize_path_lexical)
            .filter(|path| {
                path.parent().is_some() && !home.as_ref().is_some_and(|home| home.starts_with(path))
            })
            .or_else(|| home.map(|home| home.join(".trae")))
    }

    fn skills_home(&self, _env: &BTreeMap<String, String>) -> Option<PathBuf> {
        None
    }
}

impl HookCapability for TraeAdapter {}
impl InstallationCapability for TraeAdapter {}
impl SessionCapability for TraeAdapter {}
impl TranscriptCapability for TraeAdapter {}
impl ContextCapability for TraeAdapter {}
impl SpendingCapability for TraeAdapter {}
impl AccountCapability for TraeAdapter {}
impl RuntimeControlCapability for TraeAdapter {}
