//! Qoder's native hook and interactive launch adapter.

mod folder_trust;
mod install;
mod payloads;
mod settings;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, LocalSessionDiscovery, PlanLabel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::{AskKind, LifecycleSignal};
use super::{
    AgentLifecycleObservation, AgentTurnError, HookOutput, HookRouting, Result, RootIdentity,
    SanitizedPrompt, SessionOrigin, SubagentIdentity, TurnErrorClass, non_empty_trimmed,
    optional_payload_string, resolve_root_identity, resolve_subagent_identity,
};
use crate::agents::capabilities::*;
use payloads::{QoderHook, QoderStopError};

static QODER_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "qoder",
    aliases: &[],
    display_name: "Qoder",
    bin_names: &["qodercli", "qoder"],
    bin_identity: None,
    brand: Brand {
        emblem: None,
        color: 114,
        color_rgb: (0x8a, 0xe6, 0xa2),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &[
            "Bash",
            "PowerShell",
            "BashInput",
            "Monitor",
            "Write",
            "Edit",
            "NotebookEdit",
        ],
        editing: &["Write", "Edit", "NotebookEdit"],
        blocking: &[
            ("ExitPlanMode", AskKind::PlanApproval),
            ("AskUserQuestion", AskKind::Question),
        ],
    },
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
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
    coverage: CoverageAnnotations {
        turn_lifecycle: ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop/StopFailure/QueryEnd:abort",
        },
        permission: ConcernCoverage::Wired {
            via: "PermissionRequest",
        },
        plan_approval: ConcernCoverage::Wired {
            via: "PermissionRequest:ExitPlanMode",
        },
        user_question: ConcernCoverage::Wired {
            via: "PermissionRequest:AskUserQuestion",
        },
        answer: ConcernCoverage::Unsupported {
            reason: "native dialog answering not wired; answer in the pane",
        },
        compaction: ConcernCoverage::Wired {
            via: "PreCompact/PostCompact/SessionStart:compact",
        },
        subagents: ConcernCoverage::Wired {
            via: "SubagentStart/SubagentStop",
        },
        launch_reminders: ConcernCoverage::Wired {
            via: "--append-system-prompt",
        },
        background_parking: ConcernCoverage::Unsupported {
            reason: "no background wait records are integrated",
        },
        background_shells: ConcernCoverage::Unsupported {
            reason: "no background shell records are integrated",
        },
        session_end: ConcernCoverage::Wired { via: "SessionEnd" },
        idle_notification: ConcernCoverage::Wired {
            via: "Notification audit hook; no lifecycle signal",
        },
        context_usage: ConcernCoverage::Unsupported {
            reason: "ordinary Qoder token counts are hidden; no observed context schema",
        },
        realtime_cost: ConcernCoverage::Unsupported {
            reason: "token counts are hidden and Credits have no established dollar conversion",
        },
        rich_context: ConcernCoverage::Unsupported {
            reason: "statusLine schema has not been observed live",
        },
        hook_install: ConcernCoverage::Wired {
            via: "~/.qoder/settings.json",
        },
        account_spend: ConcernCoverage::Unsupported {
            reason: "no integrated login, quota, or Credits source",
        },
        tool_stats: ConcernCoverage::Unsupported {
            reason: "tool statistics are not integrated for this adapter",
        },
        remote_control: ConcernCoverage::Unsupported {
            reason: "ACP and SDK transports are outside the pane-first adapter",
        },
    },
    user_coverage: UserCoverage {
        state: CapabilityLevel::Full {
            note: "the card tracks the session from start through every turn to close",
        },
        live: CapabilityLevel::Unsupported {
            reason: "local records carry no usable token, context, or cost figures",
        },
        history: CapabilityLevel::Unsupported {
            reason: "local records carry no usable token, context, or cost figures",
        },
        account: CapabilityLevel::Unsupported {
            reason: "no readable login, plan, or credits balance reaches the card",
        },
        ask: CapabilityLevel::Full {
            note: "permission prompts, plan approvals, and questions reach rimz asks; answer in qoder",
        },
        subagents: CapabilityLevel::Partial {
            shows: "children nest under the parent card by agent type",
            limit: "per-child tokens and transcript replay are not available",
        },
    },
    lifecycle_hooks: LifecycleAnnotations {
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
    },
    default_context_window: None,
    default_model: None,
    process_names: &["qodercli", "node"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("qodercli"),
        prompt: super::PromptStyle::Flag("-i"),
        resume: Some(super::SessionCommand {
            before_id: &["qodercli", "--resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["qodercli", "--resume"],
            after_id: &["--fork-session"],
        }),
        picker: Some(&["qodercli", "--resume"]),
        permission: super::LaunchPermissionArgs {
            ask: &["--permission-mode", "default"],
            auto: &["--permission-mode", "auto"],
            plan: &["--permission-mode", "plan"],
            yolo: &["--dangerously-skip-permissions"],
        },
        compact_command: Some(super::CompactCommand {
            command: "/compact",
            instruction: super::CompactInstruction::Trailing,
        }),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--reasoning-effort"])),
            ..super::PresetMatchers::EMPTY
        },
        ..super::LaunchSpec::EMPTY
    },
};

const QODER_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle("SessionStart", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("SessionEnd", r#"{"session_id":"sess-1"}"#).session_ended(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"session_id":"sess-1","prompt":"task"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("Stop", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("StopFailure", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle("QueryEnd", r#"{"session_id":"sess-1","reason":"abort"}"#),
    HookEventSpec::lifecycle("Notification", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle("PostToolUse", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("PostToolUseFailure", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("PreCompact", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle("PostCompact", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::blocking(
        "PermissionRequest",
        r#"{"session_id":"sess-1","tool_name":"Bash"}"#,
        AskKind::Permission,
    )
    .synchronous()
    .with_lifecycle_fallback(),
];

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct QoderAdapter;

impl CoreCapability for QoderAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &QODER_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        let mut classification = super::hook_types::catalog_classification_corpus(QODER_HOOKS);
        for (tool, kind) in [
            ("AskUserQuestion", AskKind::Question),
            ("ExitPlanMode", AskKind::PlanApproval),
        ] {
            classification.push(super::ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({"session_id":"sess-1","tool_name":tool}),
                super::AgentHookClass::AwaitingUser,
                Some(kind),
            ));
        }
        super::AdapterConformance {
            classification,
            ..Default::default()
        }
    }
}

impl LaunchCapability for QoderAdapter {
    fn min_version(&self) -> Option<super::version::CliVersion> {
        Some(install::MIN_VERSION)
    }
    fn version_upgrade_fix(&self) -> Option<&'static str> {
        Some("run `qodercli update`")
    }
    fn launch_blocker(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        argv: &[String],
    ) -> Option<LaunchBlocker> {
        settings::hook_blocker(cwd, env, argv)
    }
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["QODER_CONFIG_DIR"]
    }
    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        env.get("QODER_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env.get("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|v| Path::new(v).join(".qoder"))
            })
    }
    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        Some(self.config_home(env)?.join("skills"))
    }
    fn manual_skill(&self) -> ManualSkill {
        ManualSkill::Frontmatter
    }
    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::TextFlag {
            flags: vec!["--append-system-prompt".into()],
        })
    }
}

impl HookCapability for QoderAdapter {
    fn decode_hook(&self, event: &str, payload: &Value) -> Result<HookOutput> {
        let hook = QoderHook::parse(payload);
        let ask = (event == "PermissionRequest").then(|| {
            self.spec()
                .blocking_tool_kind(hook.tool_name.as_deref())
                .unwrap_or(AskKind::Permission)
        });
        let mut decoded = decode_catalog_hook(QODER_HOOKS, event, ask);
        decoded.set_routing(
            HookRouting::split(
                optional_payload_string(payload, &["agent_id", "session_id"]).map(Into::into),
                optional_payload_string(payload, &["session_id"]).map(Into::into),
            )
            .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        if ask.is_some() {
            decoded.set_ask(Vec::new(), super::question::permission_detail(payload));
        }
        if event == "StopFailure" {
            let label = hook
                .last_assistant_message
                .as_deref()
                .and_then(crate::agents::context::cap_turn_error_label);
            let class = match hook.error {
                QoderStopError::RateLimit => TurnErrorClass::PausedRateLimit,
                QoderStopError::ServerError | QoderStopError::Overloaded => {
                    TurnErrorClass::PausedOverloaded
                }
                _ => TurnErrorClass::classify_label(label.as_deref()),
            };
            decoded.set_turn_error(Some(AgentTurnError {
                class,
                at: Timestamp::now(),
                label,
            }));
        }
        if matches!(event, "Stop" | "StopFailure" | "SubagentStop") {
            decoded.set_final_message(
                hook.last_assistant_message
                    .as_deref()
                    .and_then(non_empty_trimmed),
            );
        }
        if let Some(observation) = lifecycle(self.spec(), event, payload, &hook) {
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }
}

fn lifecycle(
    spec: &AgentSpec,
    event: &str,
    payload: &Value,
    hook: &QoderHook,
) -> Option<AgentLifecycleObservation> {
    let signal = match event {
        "SessionStart" => hook.source.session_start_signal(),
        "UserPromptSubmit" => LifecycleSignal::TurnStarted { turn_id: None },
        "Stop" | "StopFailure" => LifecycleSignal::TurnEnded {
            errored: event == "StopFailure",
            parked_on_background: false,
            turn_id: None,
        },
        "QueryEnd" if hook.reason.as_deref() == Some("abort") => {
            LifecycleSignal::TurnInterrupted { turn_id: None }
        }
        "PostToolUse" | "PostToolUseFailure" => LifecycleSignal::ToolUsed {
            mutates: event == "PostToolUse" && spec.tool_mutates(payload),
            edits: event == "PostToolUse" && spec.tool_edits_files(payload),
            name: None,
            native_key: hook.tool_use_id.clone(),
            turn_id: None,
        },
        "PermissionRequest" => LifecycleSignal::AwaitingInput {
            kind: spec
                .blocking_tool_kind(hook.tool_name.as_deref())
                .unwrap_or(AskKind::Permission),
            ask_id: None,
            detail: None,
            native_key: hook.tool_use_id.clone(),
        },
        "SubagentStart" => LifecycleSignal::SubagentStarted,
        "SubagentStop" => LifecycleSignal::SubagentStopped { errored: false },
        "PreCompact" => LifecycleSignal::Compacting,
        "PostCompact" => LifecycleSignal::CompactionEnded {
            auto: hook.trigger.auto_flag(),
            failed: false,
        },
        "SessionEnd" => LifecycleSignal::Ended,
        _ => return None,
    };
    let child_event = matches!(event, "SubagentStart" | "SubagentStop");
    let (agent_id, parent_agent_id) = if child_event {
        match resolve_subagent_identity(
            "qoder",
            event,
            hook.agent_id.as_deref(),
            hook.common.session_id.as_deref(),
            payload,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => (Some(agent_id), Some(parent_agent_id)),
            SubagentIdentity::Quarantined => return None,
        }
    } else {
        match resolve_root_identity(
            "qoder",
            event,
            hook.agent_id.as_deref(),
            hook.common.session_id.as_deref(),
        ) {
            RootIdentity::Root { agent_id } => (agent_id, None),
            RootIdentity::ForeignChild => return None,
        }
    };
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    observation.parent_agent_id = parent_agent_id;
    observation.transcript_path = optional_payload_string(payload, &["transcript_path"]);
    observation.launch.model = hook.model.clone();
    if event == "SessionStart"
        && matches!(
            hook.source,
            SessionSource::Startup | SessionSource::Clear | SessionSource::New
        )
    {
        observation.origin = Some(SessionOrigin::Fresh);
    }
    if event == "UserPromptSubmit" {
        observation.prompt = SanitizedPrompt::new(hook.prompt.as_deref());
        observation.task = observation.prompt.as_deref().map(ToOwned::to_owned);
    } else if child_event {
        observation.task = hook.agent_type.clone();
    }
    Some(observation)
}

impl InstallationCapability for QoderAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&QoderAdapter)
    }
    fn folder_trust(
        &self,
        cwd: &Path,
        _repo_root: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Option<super::FolderTrust> {
        Some(folder_trust::folder_trust(cwd, env))
    }
}
impl ContextCapability for QoderAdapter {}
impl TranscriptCapability for QoderAdapter {}
impl SpendingCapability for QoderAdapter {}
impl AccountCapability for QoderAdapter {}
impl RuntimeControlCapability for QoderAdapter {}
impl SessionCapability for QoderAdapter {}

#[cfg(test)]
mod tests;
