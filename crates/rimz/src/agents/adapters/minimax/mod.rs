//! MiniMax Code native plugin hooks.
//!
//! Hooks supply root-session, turn, tool, permission, and compaction signals. Child payloads have no parent identity, and the temporary hook transcript is not a history source. Failed and cancelled turns emit no Stop.

mod install;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agents::capabilities::*;
use crate::agents::definition::*;
use crate::agents::hook_types::{
    CompactTrigger, HookEventSpec, SessionSource, decode_catalog_hook,
};
use crate::agents::{
    AgentLifecycleObservation, AskKind, HookOutput, HookRouting, LifecycleSignal, PresetArgMatcher,
    Result, SanitizedPrompt, optional_payload_string, sanitize_user_prompt,
};
use serde_json::Value;

static MINIMAX_SPEC: AgentSpec = AgentSpec {
    kind: "minimax",
    aliases: &["mcode"],
    display_name: "MiniMax Code",
    bin_names: &["mcode"],
    bin_identity: None,
    process_names: &["minimax-code", "node"],
    extra_bin_dirs: &[],
    brand: Brand {
        emblem: None,
        color: 203,
        color_rgb: (0xff, 0x55, 0x77),
    },
    plan_label: PlanLabel::Prefixed { prefix: "MiniMax" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: Some("tool_input"),
        mutating: &["bash", "write", "edit", "task"],
        editing: &["write", "edit"],
        blocking: &[],
    },
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: true,
        local_session_discovery: LocalSessionDiscovery::Unsupported,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: crate::agents::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: CoverageAnnotations {
        turn_lifecycle: ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop; failed turns have no Stop",
        },
        permission: ConcernCoverage::Wired {
            via: "PermissionRequest; root tool approvals only",
        },
        plan_approval: ConcernCoverage::Unsupported {
            reason: "no verified plan-approval hook",
        },
        user_question: ConcernCoverage::Unsupported {
            reason: "native questions have no verified hook",
        },
        answer: ConcernCoverage::Unsupported {
            reason: "answer approvals in MiniMax Code's pane",
        },
        compaction: ConcernCoverage::Wired {
            via: "PreCompact/PostCompact/SessionStart:compact",
        },
        subagents: ConcernCoverage::Unsupported {
            reason: "child hooks carry no parent session identity",
        },
        launch_reminders: ConcernCoverage::Wired {
            via: "--append-system-prompt",
        },
        background_parking: ConcernCoverage::Unsupported {
            reason: "no verified background parking signal",
        },
        background_shells: ConcernCoverage::Unsupported {
            reason: "background shells are not mapped",
        },
        session_end: ConcernCoverage::Wired { via: "SessionEnd" },
        idle_notification: ConcernCoverage::Partial {
            via: "Stop",
            gap: "failed or cancelled turns emit no Stop",
        },
        context_usage: ConcernCoverage::Unsupported {
            reason: "token and context accounting are unverified",
        },
        realtime_cost: ConcernCoverage::Unsupported {
            reason: "token accounting and dollar semantics are unverified",
        },
        rich_context: ConcernCoverage::Partial {
            via: "hook model",
            gap: "no context gauge, effort, or durable transcript",
        },
        hook_install: ConcernCoverage::Wired {
            via: "<dataDir>/plugins/rimz native plugin",
        },
        account_spend: ConcernCoverage::Unsupported {
            reason: "account and usage schemas are unverified",
        },
        tool_stats: ConcernCoverage::Wired {
            via: "PostToolUse tool_name",
        },
        remote_control: ConcernCoverage::Unsupported {
            reason: "no verified stock-TUI remote-control surface",
        },
    },
    user_coverage: UserCoverage {
        state: CapabilityLevel::Partial {
            shows: "the card follows normal turns and root tool calls",
            limit: "failed or cancelled turns stay working until another prompt or session end",
        },
        live: CapabilityLevel::Partial {
            shows: "the selected model and tool activity appear on the card",
            limit: "no context gauge, token counts, or dollar figure",
        },
        history: CapabilityLevel::Unsupported {
            reason: "stored conversation and token schemas are not yet verified",
        },
        account: CapabilityLevel::Unsupported {
            reason: "login state, plan, and account usage are not yet verified",
        },
        ask: CapabilityLevel::Partial {
            shows: "root tool approvals raise waiting and route to the pane",
            limit: "subagent approvals and native questions are not reported",
        },
        subagents: CapabilityLevel::Unsupported {
            reason: "child hooks lack the parent identity needed for separate cards",
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
        subagent_started: HookCoverage::Absent {
            reason: "child hooks lack parent identity",
        },
        subagent_stopped: HookCoverage::Absent {
            reason: "child hooks lack parent identity",
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
    thread_key: ThreadKey::PerFile,
    launch: LaunchSpec {
        program: Some("mcode"),
        picker: Some(&["mcode", "--session"]),
        prompt: PromptStyle::PositionalAfterDoubleDash,
        resume: Some(SessionCommand {
            before_id: &["mcode", "--session"],
            after_id: &[],
        }),
        compact_command: Some(CompactCommand {
            command: "/compact",
            instruction: CompactInstruction::Trailing,
        }),
        presets: PresetMatchers {
            model: Some(StaticPresetMatcher::Flag(&["--model", "-m"])),
            system_prompt_file: Some(StaticPresetMatcher::Flag(&[
                "--append-system-prompt-file",
                "--append-system-prompt",
            ])),
            ..PresetMatchers::EMPTY
        },
        ..LaunchSpec::EMPTY
    },
};

const HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "SessionStart",
        r#"{"session_id":"mvs_root","source":"startup"}"#,
    ),
    HookEventSpec::lifecycle(
        "SessionEnd",
        r#"{"session_id":"mvs_root","reason":"logout"}"#,
    )
    .with_timeout(2)
    .session_ended(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"session_id":"mvs_root","prompt":"ping"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PreToolUse",
        r#"{"session_id":"mvs_root","tool_name":"read"}"#,
    )
    .progress(),
    HookEventSpec::blocking(
        "PermissionRequest",
        r#"{"session_id":"mvs_root","tool_name":"bash"}"#,
        AskKind::Permission,
    )
    .synchronous(),
    HookEventSpec::lifecycle(
        "PostToolUse",
        r#"{"session_id":"mvs_root","tool_name":"edit"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("Stop", r#"{"session_id":"mvs_root"}"#),
    HookEventSpec::lifecycle("PreCompact", r#"{"session_id":"mvs_root"}"#),
    HookEventSpec::lifecycle(
        "PostCompact",
        r#"{"session_id":"mvs_root","trigger":"auto"}"#,
    ),
];

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct MiniMaxAdapter;

impl CoreCapability for MiniMaxAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &MINIMAX_SPEC
    }

    #[cfg(test)]
    fn conformance(&self) -> crate::agents::AdapterConformance {
        crate::agents::AdapterConformance {
            classification: crate::agents::hook_types::catalog_classification_corpus(HOOKS),
            ..crate::agents::AdapterConformance::default()
        }
    }
}

impl LaunchCapability for MiniMaxAdapter {
    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &["MINIMAX_DATA_DIR"]
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        ["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"]
            .iter()
            .filter_map(|key| env.get(*key))
            .map(|value| value.trim())
            .find(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env.get("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".minimax"))
            })
    }

    fn skills_home(&self, _env: &BTreeMap<String, String>) -> Option<PathBuf> {
        None
    }

    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::TextFlag {
            flags: vec![
                "--append-system-prompt".into(),
                "--append-system-prompt-file".into(),
            ],
        })
    }

    fn system_prompt_arg_matchers(&self) -> Vec<PresetArgMatcher> {
        vec![
            PresetArgMatcher::Flag(vec![
                "--system-prompt-file".into(),
                "--append-system-prompt-file".into(),
            ]),
            PresetArgMatcher::TextFlag(vec![
                "--system-prompt".into(),
                "--append-system-prompt".into(),
            ]),
        ]
    }

    fn is_interactive_process(&self, command: &str) -> bool {
        let Some(words) = shlex::split(command) else {
            return false;
        };
        let program = crate::proc::command::effective_program_info(command).program;
        let start = words.iter().position(|word| word == program).unwrap_or(0);
        let mut words = words.iter().skip(start + 1);
        let is_prompt = |word: &str| {
            !matches!(
                word,
                "init"
                    | "exec"
                    | "acp"
                    | "login"
                    | "logout"
                    | "update"
                    | "telemetry"
                    | "provider"
                    | "plugin"
                    | "help"
                    | "git"
                    | "changes"
                    | "projects"
            )
        };
        while let Some(word) = words.next() {
            if word == "--" {
                return words.next().is_none_or(|word| is_prompt(word));
            }
            if matches!(
                word.as_str(),
                "--model"
                    | "-m"
                    | "--session"
                    | "--resume"
                    | "--system-prompt"
                    | "--system-prompt-file"
                    | "--append-system-prompt"
                    | "--append-system-prompt-file"
            ) {
                words.next();
                continue;
            }
            if word.starts_with('-') {
                continue;
            }
            return is_prompt(word);
        }
        true
    }
}

impl HookCapability for MiniMaxAdapter {
    fn hook_inherits_env(&self) -> bool {
        false
    }

    fn hook_env_keys(&self) -> &'static [&'static str] {
        &["MAVIS_DATA_DIR"]
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        if payload.get("agent_id").is_some() {
            return Ok(decode_catalog_hook([], event_name, None));
        }
        let mut decoded = decode_catalog_hook(
            HOOKS,
            event_name,
            (event_name == "PermissionRequest").then_some(AskKind::Permission),
        );
        let agent_id = optional_payload_string(payload, &["session_id"]).map(Into::into);
        decoded.set_routing(HookRouting::session(agent_id.clone()));
        let turn_id = optional_payload_string(payload, &["turn_id"]);
        let signal = match event_name {
            "SessionStart" => serde_json::from_value::<SessionSource>(payload["source"].clone())
                .unwrap_or_default()
                .session_start_signal(),
            "UserPromptSubmit" => LifecycleSignal::TurnStarted { turn_id },
            "PreToolUse" | "PostToolUse" => LifecycleSignal::ToolUsed {
                mutates: event_name == "PostToolUse" && self.spec().tool_mutates(payload),
                edits: event_name == "PostToolUse" && self.spec().tool_edits_files(payload),
                name: (event_name == "PostToolUse")
                    .then(|| optional_payload_string(payload, &["tool_name"]))
                    .flatten(),
                native_key: optional_payload_string(payload, &["tool_use_id"]),
                turn_id,
            },
            "PermissionRequest" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            "Stop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id,
            },
            "PreCompact" => LifecycleSignal::Compacting,
            "PostCompact" => LifecycleSignal::CompactionEnded {
                auto: serde_json::from_value::<CompactTrigger>(payload["trigger"].clone())
                    .unwrap_or_default()
                    .auto_flag(),
                failed: false,
            },
            "SessionEnd" => LifecycleSignal::Ended,
            _ => return Ok(decoded),
        };
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.launch.model = optional_payload_string(payload, &["model"]);
        if event_name == "UserPromptSubmit" {
            let prompt = optional_payload_string(payload, &["prompt"]);
            observation.task = sanitize_user_prompt(prompt.as_deref());
            observation.prompt = SanitizedPrompt::new(prompt.as_deref());
        }
        if event_name == "Stop" {
            decoded.set_final_message(optional_payload_string(
                payload,
                &["last_assistant_message"],
            ));
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl InstallationCapability for MiniMaxAdapter {
    fn managed_integration(&self) -> Option<&'static dyn crate::agents::ManagedIntegration> {
        Some(&install::INTEGRATION)
    }
}

impl AccountCapability for MiniMaxAdapter {}
impl ContextCapability for MiniMaxAdapter {}
impl RuntimeControlCapability for MiniMaxAdapter {}
impl SessionCapability for MiniMaxAdapter {}
impl SpendingCapability for MiniMaxAdapter {}
impl TranscriptCapability for MiniMaxAdapter {}
