//! Hermes classic CLI shell-hook adapter.

mod install;
#[cfg(test)]
mod tests;

use super::*;
use crate::agents::capabilities::*;
use crate::agents::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, LocalSessionDiscovery, PlanLabel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use crate::agents::hook_types::{HookEventSpec, SessionSource, decode_catalog_hook};
use crate::ids::AgentSessionId;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

static HERMES_SPEC: AgentSpec = AgentSpec {
    kind: "hermes",
    aliases: &[],
    bin_names: &["hermes"],
    bin_identity: None,
    display_name: "Hermes",
    brand: Brand {
        emblem: None,
        color: 214,
        color_rgb: (0xff, 0xa5, 0x00),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Hermes" },
    sub_providers: &[],
    expected_windows: &[],
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    tools: ToolClassification {
        input_key: None,
        mutating: &["write_file", "patch", "terminal"],
        editing: &["write_file", "patch"],
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
        same_pane_session: SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: CoverageAnnotations {
        turn_lifecycle: ConcernCoverage::Wired {
            via: "on_session_start/pre_llm_call/on_session_end",
        },
        permission: ConcernCoverage::Partial {
            via: "on_human_input_request approval/sudo brackets",
            gap: "memory, secret, vault, and confirmation panels have no bracket",
        },
        plan_approval: ConcernCoverage::Unsupported {
            reason: "no enforced plan-approval gate",
        },
        user_question: ConcernCoverage::Partial {
            via: "on_human_input_request clarify brackets",
            gap: "other native panels have no bracket",
        },
        answer: ConcernCoverage::Unsupported {
            reason: "answer native prompts in Hermes's pane",
        },
        compaction: ConcernCoverage::Unsupported {
            reason: "no committed compaction bracket; rotating compression changes session identity silently",
        },
        subagents: ConcernCoverage::Partial {
            via: "subagent_start/subagent_stop child session IDs",
            gap: "stops arrive during result processing, not at the child's last token",
        },
        launch_reminders: ConcernCoverage::Unsupported {
            reason: "HERMES_EPHEMERAL_SYSTEM_PROMPT replaces the user's personality and system text",
        },
        background_parking: ConcernCoverage::Unsupported {
            reason: "no background parking signal",
        },
        background_shells: ConcernCoverage::Unsupported {
            reason: "background shells are not mapped",
        },
        session_end: ConcernCoverage::Partial {
            via: "rimz exec wrapper",
            gap: "on_session_finalize cannot distinguish exit from /new",
        },
        idle_notification: ConcernCoverage::Partial {
            via: "on_session_end",
            gap: "no idle notification hook",
        },
        context_usage: ConcernCoverage::Wired {
            via: "post_api_request usage.prompt_tokens and context_length",
        },
        realtime_cost: ConcernCoverage::Unsupported {
            reason: "tokens do not establish billed dollars",
        },
        rich_context: ConcernCoverage::Partial {
            via: "post_api_request model and context gauge",
            gap: "no transcript, effort, or cost enrichment",
        },
        hook_install: ConcernCoverage::Wired {
            via: "$HERMES_HOME/config.yaml shell hooks",
        },
        account_spend: ConcernCoverage::Unsupported {
            reason: "no validated read-only history or billing query",
        },
        tool_stats: ConcernCoverage::Unsupported {
            reason: "tool statistics are not integrated for this adapter",
        },
        remote_control: ConcernCoverage::Unsupported {
            reason: "no validated classic-CLI remote control",
        },
    },
    user_coverage: UserCoverage {
        state: CapabilityLevel::Full {
            note: "the card follows each turn and completed tool call",
        },
        live: CapabilityLevel::Partial {
            shows: "the model and last request's context fill",
            limit: "no dollar figure or transcript enrichment",
        },
        history: CapabilityLevel::Unsupported {
            reason: "Hermes's session database has no validated read-only history query",
        },
        account: CapabilityLevel::Unsupported {
            reason: "no validated account or billing source",
        },
        ask: CapabilityLevel::Partial {
            shows: "approvals, sudo, and clarify questions raise Waiting and route to the pane",
            limit: "other panels are invisible; answer in Hermes's own UI",
        },
        subagents: CapabilityLevel::Partial {
            shows: "child sessions open and close on the card",
            limit: "completion arrives when Hermes processes the child's result",
        },
    },
    lifecycle_hooks: LifecycleAnnotations {
        registered: HookCoverage::Native {
            event: "on_session_start",
        },
        turn_started: HookCoverage::Native {
            event: "pre_llm_call",
        },
        turn_ended: HookCoverage::Native {
            event: "on_session_end",
        },
        tool_used: HookCoverage::Native {
            event: "post_tool_call",
        },
        awaiting_input: HookCoverage::Derived {
            via: "on_human_input_request",
            gap: "memory, secret, vault, and confirmation panels emit no bracket",
        },
        subagent_started: HookCoverage::Native {
            event: "subagent_start",
        },
        subagent_stopped: HookCoverage::Native {
            event: "subagent_stop",
        },
        compacting: HookCoverage::Absent {
            reason: "no committed compaction bracket",
        },
        compaction_ended: HookCoverage::Absent {
            reason: "no committed compaction bracket",
        },
        ended: HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "finalize also fires on /new; no unambiguous native exit",
        },
        lost: HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "native hooks do not report mux-session death",
        },
    },
    default_context_window: None,
    default_model: None,
    process_names: &["hermes"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: LaunchSpec {
        program: Some("hermes"),
        fixed_args: &["chat", "--cli"],
        prompt: PromptStyle::JoinedFlag("--query"),
        resume: Some(SessionCommand {
            before_id: &["hermes", "chat", "--cli", "--resume"],
            after_id: &[],
        }),
        permission: LaunchPermissionArgs {
            yolo: &["--yolo"],
            ..LaunchPermissionArgs::EMPTY
        },
        compact_command: Some(CompactCommand {
            command: "/compress",
            instruction: CompactInstruction::Trailing,
        }),
        presets: PresetMatchers {
            model: Some(StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(StaticPresetMatcher::ValueFlag {
                flags: &["--reasoning"],
                values: &[
                    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                ],
            }),
            ..PresetMatchers::EMPTY
        },
        ..LaunchSpec::EMPTY
    },
};

const HERMES_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle("on_session_start",r#"{"session_id":"parent","extra":{}}"#).progress(),
    HookEventSpec::lifecycle("on_session_reset",r#"{"session_id":"new","extra":{"reason":"new_session"}}"#).progress(),
    HookEventSpec::lifecycle("pre_llm_call",r#"{"session_id":"parent","extra":{"user_message":"task"}}"#).progress(),
    HookEventSpec::lifecycle("post_tool_call",r#"{"session_id":"parent","tool_name":"write_file","extra":{}}"#).progress(),
    HookEventSpec::lifecycle("on_session_end",r#"{"session_id":"parent","extra":{"completed":true}}"#).progress(),
    HookEventSpec::lifecycle("post_api_request",r#"{"session_id":"parent","extra":{"model":"model","usage":{"prompt_tokens":100},"context_length":1000}}"#).progress(),
    HookEventSpec::lifecycle("on_human_input_request",r#"{"session_id":"parent","extra":{"kind":"approval","request_id":"r"}}"#),
    HookEventSpec::lifecycle("on_human_input_resolved",r#"{"session_id":"parent","extra":{"request_id":"r"}}"#).progress(),
    HookEventSpec::lifecycle("subagent_start",r#"{"session_id":"parent","extra":{"child_session_id":"child","child_goal":"task"}}"#),
    HookEventSpec::lifecycle("subagent_stop",r#"{"session_id":"parent","extra":{"child_session_id":"child","child_status":"completed"}}"#),
];

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct HermesAdapter;

impl CoreCapability for HermesAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &HERMES_SPEC
    }

    #[cfg(test)]
    fn conformance(&self) -> AdapterConformance {
        AdapterConformance {
            classification: crate::agents::hook_types::catalog_classification_corpus(HERMES_HOOKS),
            ..AdapterConformance::default()
        }
    }
}
impl HookCapability for HermesAdapter {
    fn decode_hook(&self, event: &str, payload: &Value) -> Result<HookOutput> {
        let mut output = decode_catalog_hook(HERMES_HOOKS, event, None);
        let mut agent_id = optional_payload_string(payload, &["session_id"])
            .filter(|id| !id.trim().is_empty())
            .map(AgentSessionId::from);
        output.set_routing(HookRouting::session(agent_id.clone()));
        let extra = &payload["extra"];
        let signal = match event {
            "on_session_start" => SessionSource::Startup.session_start_signal(),
            "on_session_reset" => SessionSource::Clear.session_start_signal(),
            "pre_llm_call" => LifecycleSignal::TurnStarted { turn_id: None },
            "post_tool_call" => LifecycleSignal::ToolUsed {
                mutates: self.spec().tool_mutates(payload),
                edits: self.spec().tool_edits_files(payload),
                name: optional_payload_string(payload, &["tool_name"]),
                // Keyless tools clear keyed asks. Only the human-input resolve
                // bracket may close Hermes's native prompt, not a sibling tool.
                native_key: Some("hermes:post_tool_call".into()),
                turn_id: None,
            },
            "on_session_end" => LifecycleSignal::TurnEnded {
                errored: extra["failed"].as_bool().unwrap_or(false),
                parked_on_background: false,
                turn_id: None,
            },
            "on_human_input_request" => LifecycleSignal::AwaitingInput {
                kind: match extra["kind"].as_str() {
                    Some("approval" | "sudo") => AskKind::Permission,
                    _ => AskKind::Question,
                },
                ask_id: None,
                detail: None,
                native_key: optional_payload_string(extra, &["request_id"]),
            },
            "on_human_input_resolved" => LifecycleSignal::AskResolved {
                native_key: optional_payload_string(extra, &["request_id"]),
            },
            "post_api_request" => {
                if let Some(agent_id) = agent_id {
                    let tokens = crate::agents::context::AgentTokenUsage {
                        current_context_tokens: extra
                            .pointer("/usage/prompt_tokens")
                            .and_then(Value::as_u64),
                        context_window_size: extra["context_length"].as_u64(),
                        ..Default::default()
                    };
                    let context = crate::agents::context::AgentContext {
                        model_id: optional_payload_string(extra, &["model"]),
                        tokens: Some(tokens),
                        ..crate::agents::context::AgentContext::new(
                            "hermes",
                            jiff::Timestamp::now(),
                        )
                    };
                    output.set_observed_context(crate::agents::context::ContextObservation::new(
                        agent_id, context,
                    ));
                }
                return Ok(output);
            }
            "subagent_start" | "subagent_stop" => {
                let (child, parent) = match resolve_subagent_identity(
                    "hermes",
                    event,
                    extra["child_session_id"].as_str(),
                    agent_id.as_deref(),
                    payload,
                ) {
                    SubagentIdentity::Resolved {
                        agent_id,
                        parent_agent_id,
                    } => (agent_id, parent_agent_id),
                    SubagentIdentity::Quarantined => return Ok(output),
                };
                let signal = if event == "subagent_start" {
                    LifecycleSignal::SubagentStarted
                } else {
                    LifecycleSignal::SubagentStopped {
                        errored: matches!(
                            extra["child_status"].as_str(),
                            Some("failed" | "error" | "timeout")
                        ),
                    }
                };
                let mut observation = AgentLifecycleObservation::new(Some(child), signal)
                    .with_worktree_from_payload(payload);
                observation.parent_agent_id = Some(parent);
                if event == "subagent_start" {
                    observation.task = optional_payload_string(extra, &["child_goal"])
                        .and_then(|task| sanitize_user_prompt(Some(&task)));
                }
                output.attach_lifecycle(observation);
                return Ok(output);
            }
            _ => return Ok(output),
        };
        let mut observation = AgentLifecycleObservation::new(agent_id.take(), signal)
            .with_worktree_from_payload(payload);
        if event == "pre_llm_call" {
            let prompt = optional_payload_string(extra, &["user_message"]);
            observation.task = sanitize_user_prompt(prompt.as_deref());
            observation.prompt = SanitizedPrompt::new(prompt.as_deref());
        }
        output.attach_lifecycle(observation);
        Ok(output)
    }
}
impl InstallationCapability for HermesAdapter {
    fn managed_integration(&self) -> Option<&'static dyn ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
    fn hook_trust_fix(&self) -> String {
        "launch hermes in a terminal and answer y to each RimZ hook prompt".into()
    }
    fn hook_trust_choice(&self) -> Option<&'static str> {
        Some("answer y to each RimZ hook prompt")
    }
}
impl LaunchCapability for HermesAdapter {
    fn min_version(&self) -> Option<crate::agents::version::CliVersion> {
        Some(install::MIN_VERSION)
    }
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["HERMES_HOME"]
    }
    fn supports_named_logins(&self) -> bool {
        false
    }
    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        let home = env
            .get("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| crate::utils::path::normalize_path_lexical(Path::new(home)));
        env.get("HERMES_HOME")
            .filter(|value| !value.is_empty())
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .map(crate::utils::path::normalize_path_lexical)
            .filter(|path| {
                path.parent().is_some() && !home.as_ref().is_some_and(|home| home.starts_with(path))
            })
            .or_else(|| home.map(|home| home.join(".hermes")))
    }
    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        Some(self.config_home(env)?.join("skills"))
    }
}
impl SessionCapability for HermesAdapter {}
impl TranscriptCapability for HermesAdapter {}
impl ContextCapability for HermesAdapter {}
impl SpendingCapability for HermesAdapter {}
impl AccountCapability for HermesAdapter {}
impl RuntimeControlCapability for HermesAdapter {}
