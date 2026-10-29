//! Muse Code 1.4.9 managed hooks and native turn lifecycle.

mod install;
mod payloads;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::agents::capabilities::*;
use crate::agents::definition::{
    Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations, HookCoverage,
    LifecycleAnnotations, LocalSessionDiscovery, PlanLabel, RemoteControlCapability,
    SamePaneSessionPolicy, ThreadKey, ToolClassification, UserCoverage,
};
use crate::agents::hook_types::{HookEventSpec, decode_catalog_hook};
use crate::agents::{
    AgentLifecycleObservation, AgentSpec, AskKind, HookIngressAcceptance, HookIngressDecision,
    HookIngressIgnoreReason, HookOutput, HookRouting, LifecycleSignal, Result, SanitizedPrompt,
};
use crate::ids::AgentSessionId;

static MUSE_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "muse",
    aliases: &[],
    display_name: "Muse",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x08, 0x66, 0xff),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Muse" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &["shell"],
        editing: &[],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: true,
        local_session_discovery: LocalSessionDiscovery::Unsupported,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: MUSE_COVERAGE,
    user_coverage: MUSE_USER_COVERAGE,
    lifecycle_hooks: MUSE_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["muse-bin-<version>"],
    bin_names: &["muse"],
    bin_identity: None,
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    launch: crate::agents::LaunchSpec {
        program: Some("muse"),
        prompt: crate::agents::PromptStyle::PositionalAfterDoubleDash,
        picker: Some(&["muse", "resume"]),
        resume: Some(crate::agents::SessionCommand {
            before_id: &["muse", "resume"],
            after_id: &[],
        }),
        permission: crate::agents::LaunchPermissionArgs {
            auto: &["--permission-profile", ":auto-review"],
            ask: &["--permission-profile", ":ask-me"],
            yolo: &["--yolo"],
            plan: &["--permission-profile", ":read-only"],
        },
        compact_command: Some(crate::agents::CompactCommand {
            command: "/compact",
            instruction: crate::agents::CompactInstruction::Unsupported,
        }),
        presets: crate::agents::PresetMatchers {
            model: Some(crate::agents::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(crate::agents::StaticPresetMatcher::Flag(&[
                "--reasoning-effort",
            ])),
            ..crate::agents::PresetMatchers::EMPTY
        },
        ..crate::agents::LaunchSpec::EMPTY
    },
};

const MUSE_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop/StopFailure; Interrupt cancels",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest (documented shape; native timing unverified)",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no mapped plan-approval event",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "clarifying questions have no captured hook",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "answer in the native Muse pane",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "compaction hooks remain uncaptured",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "only internal reminder children have been captured",
    },
    launch_reminders: ConcernCoverage::Unsupported {
        reason: "no additive system-text launch channel is implemented",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal is mapped",
    },
    background_shells: ConcernCoverage::Unsupported {
        reason: "background shells are not mapped",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Unsupported {
        reason: "delayed Notification is not installed",
    },
    context_usage: ConcernCoverage::Unsupported {
        reason: "no captured nonzero usage or context-window source",
    },
    realtime_cost: ConcernCoverage::Unsupported {
        reason: "no captured dollar usage or model price",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "native session-store schema is unpublished",
    },
    hook_install: ConcernCoverage::Wired {
        via: "managed_hooks_path and managed_hooks_env_vars in settings.json",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "native session-store schema and real usage are unverified",
    },
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no native remote-control surface is mapped",
    },
};

const MUSE_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card follows turns, tool calls, approvals, and session endings",
    },
    live: CapabilityLevel::Unsupported {
        reason: "no verified live context gauge or dollar usage source",
    },
    history: CapabilityLevel::Unsupported {
        reason: "saved sessions use an unpublished format without verified usage",
    },
    account: CapabilityLevel::Unsupported {
        reason: "no readable login, plan, or balance source is integrated",
    },
    ask: CapabilityLevel::Partial {
        shows: "tool approvals raise Waiting and route to Muse's pane",
        limit: "answer in Muse's UI; clarifying questions do not raise Waiting",
    },
    subagents: CapabilityLevel::Unsupported {
        reason: "only internal reminder children have been observed so far",
    },
};

const MUSE_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
    subagent_started: HookCoverage::Absent {
        reason: "model-spawned children remain uncaptured",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "model-spawned children remain uncaptured",
    },
    compacting: HookCoverage::Absent {
        reason: "compaction hooks remain uncaptured",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "compaction hooks remain uncaptured",
    },
    ended: HookCoverage::Native {
        event: "SessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const MUSE_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle("SessionStart", include_str!("fixtures/SessionStart.json")).progress(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        include_str!("fixtures/UserPromptSubmit.json"),
    )
    .progress(),
    // Model-tool samples are documented-shape; captured user-shell samples are tested separately.
    HookEventSpec::lifecycle("PreToolUse", include_str!("fixtures/ModelPreToolUse.json"))
        .progress(),
    // Documented-shape fixtures, not captured from a native approval or failure.
    HookEventSpec::blocking(
        "PermissionRequest",
        include_str!("fixtures/PermissionRequest.json"),
        AskKind::Permission,
    )
    .synchronous(),
    HookEventSpec::lifecycle(
        "PostToolUse",
        include_str!("fixtures/ModelPostToolUse.json"),
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PostToolUseFailure",
        include_str!("fixtures/ModelPostToolUseFailure.json"),
    )
    .progress(),
    HookEventSpec::lifecycle("Stop", include_str!("fixtures/Stop.json")).progress(),
    HookEventSpec::lifecycle("StopFailure", include_str!("fixtures/StopFailure.json")).progress(),
    HookEventSpec::lifecycle("Interrupt", include_str!("fixtures/Interrupt.json")).progress(),
    HookEventSpec::lifecycle("SessionEnd", include_str!("fixtures/SessionEnd.json"))
        .session_ended(),
];

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct MuseAdapter;

impl CoreCapability for MuseAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &MUSE_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> crate::agents::AdapterConformance {
        crate::agents::AdapterConformance {
            classification: crate::agents::hook_types::catalog_classification_corpus(MUSE_HOOKS),
            ..Default::default()
        }
    }
}

impl LaunchCapability for MuseAdapter {
    fn min_version(&self) -> Option<crate::agents::version::CliVersion> {
        Some(install::MIN_SUPPORTED_CLI)
    }

    fn version_upgrade_fix(&self) -> Option<&'static str> {
        Some("update Muse to 1.4.9 or later")
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        env.get("XDG_CONFIG_HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env.get("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .map(|home| home.join("muse"))
    }

    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        Some(self.config_home(env)?.join("skills"))
    }

    fn disable_native_sandbox_args(&self, args: &mut Vec<String>) {
        if !args.iter().any(|arg| arg == "--disable-sandbox") {
            args.push("--disable-sandbox".to_owned());
        }
    }
}

impl HookCapability for MuseAdapter {
    fn hook_env_keys(&self) -> &'static [&'static str] {
        install::passthrough_names()
    }

    fn hook_ingress(&self, pid: Option<u32>) -> HookIngressDecision {
        let root = std::env::var("MUSE_SESSION_ID").ok();
        let thread = std::env::var("MUSE_THREAD_ID").ok();
        if root
            .zip(thread)
            .is_some_and(|(root, thread)| root != thread)
        {
            return HookIngressDecision::Ignore(HookIngressIgnoreReason::MuseChildThread);
        }
        HookIngressDecision::Accept(HookIngressAcceptance::agent(pid))
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let parsed = payloads::parse(payload);
        let ask = (event_name == "PermissionRequest").then_some(AskKind::Permission);
        let mut decoded = decode_catalog_hook(MUSE_HOOKS, event_name, ask);
        let agent_id = parsed.session_id.map(AgentSessionId::from);
        decoded
            .set_routing(HookRouting::session(agent_id.clone()).with_worktree(parsed.cwd.clone()));
        let signal = match event_name {
            "SessionStart" => parsed.source.session_start_signal(),
            "UserPromptSubmit" => LifecycleSignal::TurnStarted {
                turn_id: parsed.turn_id,
            },
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
                if parsed
                    .tool_use_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("user_shell_")) =>
            {
                return Ok(decoded);
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => LifecycleSignal::ToolUsed {
                mutates: self.spec().tool_mutates(payload),
                edits: self.spec().tool_edits_files(payload),
                name: parsed.tool_name,
                native_key: parsed.tool_use_id,
                turn_id: parsed.turn_id,
            },
            "PermissionRequest" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            "Stop" | "StopFailure" => LifecycleSignal::TurnEnded {
                errored: event_name == "StopFailure",
                parked_on_background: false,
                turn_id: parsed.turn_id,
            },
            "Interrupt" => LifecycleSignal::TurnInterrupted {
                turn_id: parsed.turn_id,
            },
            "SessionEnd" => LifecycleSignal::Ended,
            _ => return Ok(decoded),
        };
        let mut observation = AgentLifecycleObservation::new(agent_id, signal);
        observation.worktree_path = parsed.cwd;
        if event_name == "UserPromptSubmit" {
            observation.prompt = SanitizedPrompt::new(parsed.prompt.as_deref());
        }
        if matches!(event_name, "Stop" | "StopFailure") {
            decoded.set_final_message(parsed.last_assistant_message);
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl InstallationCapability for MuseAdapter {
    fn managed_integration(&self) -> Option<&'static dyn crate::agents::ManagedIntegration> {
        Some(&install::MuseManagedIntegration)
    }
}

impl SessionCapability for MuseAdapter {}
impl TranscriptCapability for MuseAdapter {}
impl ContextCapability for MuseAdapter {}
impl AccountCapability for MuseAdapter {}
impl SpendingCapability for MuseAdapter {}
impl RuntimeControlCapability for MuseAdapter {}
