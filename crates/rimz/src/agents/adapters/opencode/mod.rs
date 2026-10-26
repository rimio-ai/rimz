//! OpenCode 2 hook adapter.
//!
//! RimZ launches OpenCode with `--standalone`. Its private serve child loads
//! `index.ts` through `{ id, setup }`; the plugin reads session and model
//! context through the v2 context API, stamps its parent PID as the pane TUI,
//! then shells out to
//! `rimz hooks feed --source opencode`, posts a RimZ-owned snake_case payload
//! on stdin. Current permission and question prompts arrive through
//! `permission.asked` and `form.created` events, and the plugin never
//! reads RimZ's stdout. Native reply bus events record
//! answers and clear waiting after the user responds in OpenCode's own TUI.
//! A root `session_idle` after a plan-agent turn derives a native plan-approval
//! wait that the next prompt clears after the user switches modes in the TUI.
//! `/new` registers a fresh root inside the same live process; follow-latest
//! succession hands pane and card ownership to the new conversation.
//! `session.deleted` and the activation cleanup sweep normalize to one
//! per-session `session_ended` event, with pane liveness as the crash backstop.
//! Plain shared-service clients use `tui.ts` instead: the selected conversation
//! carries an on-screen stamp, arrivals report state without lifecycle edges,
//! and background tabs report native asks, replies and deletion of an ask's session.

mod account;
mod database;
pub(in crate::agents) mod go_usage;
mod payloads;
mod session_step;
mod spend;
mod transcript;

use crate::agents::capabilities::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::AskKind;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RemoteControlCapability, ThreadKey,
    ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::managed_source::RIMZ_MANAGED_MARKER;
use super::pricing::PriceBook;
use super::{
    AgentContext, AgentErr, AgentLifecycleObservation, HookInstallFilePreview,
    HookInstallFileReport, HookOutput, HookRouting, Result, SanitizedPrompt, SubagentIdentity,
    optional_payload_string, read_optional_file, resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::agents::PermissionMode;
use crate::ids::AgentSessionId;
use crate::transcript::AskAnswer;

static OPENCODE_DESCRIPTOR: AgentSpec = AgentSpec {
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    kind: "opencode",
    aliases: &[],
    display_name: "Open Code",
    brand: Brand {
        emblem: None,
        color: 208,
        color_rgb: (0xff, 0x87, 0x00),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &["bash", "edit", "write", "apply_patch", "patch"],
        editing: &["edit", "write", "apply_patch", "patch"],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: true,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::FollowLatest,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: OPENCODE_COVERAGE,
    user_coverage: OPENCODE_USER_COVERAGE,
    lifecycle_hooks: OPENCODE_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["opencode", "bun"],
    bin_names: &["opencode"],
    bin_identity: None,
    extra_bin_dirs: &[".opencode/bin"],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        definitions: crate::agents::definition::DefinitionSpec {
            runs_other_providers_models: true,
            ..crate::agents::definition::DefinitionSpec::EMPTY
        },
        restores_session_model: true,
        model_session_step: true,
        program: Some("opencode"),
        fixed_args: &["--standalone"],
        prompt: super::PromptStyle::Flag("--prompt"),
        resume: Some(super::SessionCommand {
            before_id: &["opencode", "--session"],
            after_id: &["--standalone"],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &[],
            yolo: &["--auto"],
            plan: &[],
        },
        max_turn_flag: None,
        interrupt_key: None,
        compact_command: Some(super::CompactCommand {
            command: "/compact",
            instruction: super::CompactInstruction::Unsupported,
        }),
        presets: super::PresetMatchers {
            system_prompt_file: Some(super::StaticPresetMatcher::EnvConfigMember {
                var: "OPENCODE_CONFIG_CONTENT",
                member: "agents.build.system",
            }),
            ..super::PresetMatchers::EMPTY
        },
        ..super::LaunchSpec::EMPTY
    },
};

const OPENCODE_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "session_created/chat_message/session_idle",
    },
    permission: ConcernCoverage::Wired {
        via: "permission_ask",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "session_idle + resting plan-agent turn",
    },
    user_question: ConcernCoverage::Wired {
        via: "form.created",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native answers are observed; RimZ-to-OpenCode answer transport is not mapped",
    },
    compaction: ConcernCoverage::Wired {
        via: "session_compacting/session_compacted",
    },
    subagents: ConcernCoverage::Wired {
        via: "SubagentStart/SubagentStop",
    },
    launch_reminders: ConcernCoverage::Wired {
        via: "plugin session context hook",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    background_shells: ConcernCoverage::Unsupported {
        reason: "background shells are not mapped",
    },
    session_end: ConcernCoverage::Wired {
        via: "session_ended (session.deleted + dispose sweep)",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + permission.asked/form.created + stall window",
        gap: "no idle Notification hook; no idle-timeout nudge",
    },
    context_usage: ConcernCoverage::Wired {
        via: "session.step.ended token split",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "authoritative per-session SQLite message spend sum, reconciled at each turn boundary",
    },
    rich_context: ConcernCoverage::Wired {
        via: "in-process plugin session metadata + model catalog",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.config/opencode/plugins/rimz/{index.ts,tui.ts}; plain clients follow the screen",
    },
    account_spend: ConcernCoverage::Wired {
        via: "SQLite session_message + credential OAuth usage probe",
    },
    tool_stats: ConcernCoverage::Wired {
        via: "tool_after names + SQLite inline tool content",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const OPENCODE_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "plain shared-service clients follow the on-screen conversation; background asks take the card until answered; home keeps the last card unless deleted",
    },
    live: CapabilityLevel::Full {
        note: "the selected conversation's token split and session dollars; off-screen cost waits for its next on-screen turn end",
    },
    history: CapabilityLevel::Full {
        note: "stored sessions read end to end, with OpenCode's recorded per-turn tokens and dollars",
    },
    account: CapabilityLevel::Full {
        note: "login and plan with the backing provider's windows, their fill, reset, and credits",
    },
    ask: CapabilityLevel::Full {
        note: "permissions, plans, and questions raise Waiting and reach rimz asks with their options",
    },
    subagents: CapabilityLevel::Full {
        note: "child sessions nest under the parent as they start, with their task and usage",
    },
};

const OPENCODE_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Native {
        event: "session_created",
    },
    turn_started: HookCoverage::Native {
        event: "chat_message",
    },
    turn_ended: HookCoverage::Native {
        event: "session_idle",
    },
    tool_used: HookCoverage::Native {
        event: "tool_after",
    },
    awaiting_input: HookCoverage::Native {
        // One representative installed event names the signal, the Codex
        // precedent; `question_ask` is the other awaiting-user event and
        // rides the separately-wired `UserQuestion` concern.
        event: "permission_ask",
    },
    subagent_started: HookCoverage::Native {
        event: "SubagentStart",
    },
    subagent_stopped: HookCoverage::Native {
        event: "SubagentStop",
    },
    compacting: HookCoverage::Native {
        event: "session_compacting",
    },
    compaction_ended: HookCoverage::Native {
        event: "session_compacted",
    },
    ended: HookCoverage::Native {
        event: "session_ended",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const OPENCODE_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "session_created",
        r#"{"session_id":"ses_1","cwd":"/tmp/repo"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "chat_message",
        r#"{"session_id":"ses_1","prompt":"fix auth"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("session_idle", r#"{"session_id":"ses_1"}"#).progress(),
    HookEventSpec::lifecycle(
        "session_error",
        r#"{"session_id":"ses_1","error_message":"boom"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("tool_after", r#"{"session_id":"ses_1","tool_name":"bash"}"#)
        .progress(),
    HookEventSpec::lifecycle("session_compacting", r#"{"session_id":"ses_1"}"#),
    HookEventSpec::lifecycle("session_compacted", r#"{"session_id":"ses_1"}"#),
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"session_id":"ses_child","parent_session_id":"ses_parent","prompt":"review auth"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"session_id":"ses_child","parent_session_id":"ses_parent"}"#,
    )
    .progress(),
    HookEventSpec::blocking(
        "permission_ask",
        r#"{"session_id":"ses_1","tool_name":"bash"}"#,
        AskKind::Permission,
    )
    .synchronous(),
    HookEventSpec::blocking(
        "question_ask",
        r#"{"session_id":"ses_1","title":"Which database?"}"#,
        AskKind::Question,
    )
    .synchronous(),
    HookEventSpec::lifecycle(
        "permission_replied",
        r#"{"session_id":"ses_1","reply":"once"}"#,
    ),
    HookEventSpec::lifecycle(
        "question_replied",
        r#"{"session_id":"ses_1","answers":[["Postgres"]]}"#,
    ),
    HookEventSpec::lifecycle("question_rejected", r#"{"session_id":"ses_1"}"#),
    HookEventSpec::lifecycle(
        "session_ended",
        r#"{"session_id":"ses_1","reason":"deleted"}"#,
    )
    .session_ended(),
];

const PLUGIN_SOURCE: &str = include_str!("index.ts");
const TUI_SOURCE: &str = include_str!("tui.ts");

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct OpencodeAdapter;

impl crate::agents::capabilities::CoreCapability for OpencodeAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &OPENCODE_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        use super::{AgentHookClass, ClassificationSample};

        let mut samples = super::hook_types::catalog_classification_corpus(OPENCODE_HOOKS);
        samples.push(ClassificationSample::new(
            "session_idle",
            json!({ "session_id": "ses_1", "plan_proposed": true }),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
        ));
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "ses_1",
                file_name: "opencode.db",
                body: super::SpendFixtureBody::OpencodeSqlite {
                    data: r#"{"cost":0.42,"model":{"id":"gpt-5","providerID":"openai"},"time":{"created":1780394400000},"tokens":{"input":100,"output":50}}"#,
                },
            }),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::LaunchCapability for OpencodeAdapter {
    fn session_step(
        &self,
        request: &SessionStepRequest<'_>,
    ) -> Option<std::result::Result<Vec<String>, SessionStepRefusal>> {
        Some(session_step::run(request))
    }

    fn min_version(&self) -> Option<super::version::CliVersion> {
        Some(super::version::CliVersion::new(2, 0, 0))
    }

    fn version_upgrade_fix(&self) -> Option<&'static str> {
        Some("run `opencode upgrade --major`")
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        opencode_config_home(
            env.get("XDG_CONFIG_HOME").map(std::ffi::OsStr::new),
            env.get("HOME").map(std::ffi::OsStr::new),
        )
    }

    fn append_system_text_channel(&self) -> Option<crate::agents::capabilities::SystemTextChannel> {
        Some(crate::agents::capabilities::SystemTextChannel::ExtensionEnv)
    }

    fn finalize_launch_env(
        &self,
        login_env: &BTreeMap<String, String>,
        env: &mut BTreeMap<String, String>,
        disable_native_subagents: bool,
        system_prompt_file: Option<&Path>,
    ) -> Vec<String> {
        let mut warnings =
            edit_core_config(login_env, env, disable_native_subagents, system_prompt_file);
        warnings.extend(hide_tabs(login_env, env));
        warnings
    }
}

/// Writes `OPENCODE_CONFIG_CONTENT` when this launch switches native delegation off or
/// delivers a system prompt, keeping every inherited key. Otherwise the inherited value,
/// valid or not, passes through untouched.
fn edit_core_config(
    login_env: &BTreeMap<String, String>,
    env: &mut BTreeMap<String, String>,
    disable_native_subagents: bool,
    system_prompt_file: Option<&Path>,
) -> Vec<String> {
    const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
    const SELECTOR: &str = "-opencode.tool.subagent";
    if !disable_native_subagents && system_prompt_file.is_none() {
        return Vec::new();
    }
    let mut members: Vec<InlineConfigMember> = Vec::new();
    if disable_native_subagents {
        members.push(("/plugins", "an array", Value::is_array));
    }
    if system_prompt_file.is_some() {
        members.push(("/agents", "an object", Value::is_object));
        members.push(("/agents/build", "an object", Value::is_object));
    }
    let (mut config, warning) = parse_inline_config(
        CONFIG_ENV,
        login_env.get(CONFIG_ENV).map(String::as_str),
        &members,
    );
    let mut warnings = Vec::from_iter(warning);
    if disable_native_subagents {
        let mut plugins = match config.remove("plugins") {
            Some(Value::Array(plugins)) => plugins,
            _ => Vec::new(),
        };
        if plugins.last().and_then(Value::as_str) != Some(SELECTOR) {
            plugins.push(Value::String(SELECTOR.to_owned()));
        }
        config.insert("plugins".to_owned(), Value::Array(plugins));
    }
    let mut config = Value::Object(config);
    if let Some(path) = system_prompt_file {
        if config.pointer("/agents/build/system").is_some() {
            warnings.push(
                "OPENCODE_CONFIG_CONTENT set agents.build.system; RimZ's composed system prompt \
                 replaced it for this launch; move that prompt into agents/opencode.md or a \
                 profile's system-prompt-file, or remove it from the variable"
                    .to_owned(),
            );
        }
        // parse_inline_config admitted agents and agents.build only as objects or absent,
        // so indexing creates the missing levels and cannot hit a non-object.
        config["agents"]["build"]["system"] = Value::String(format!("{{file:{}}}", path.display()));
    }
    env.insert(CONFIG_ENV.to_owned(), config.to_string());
    warnings
}

/// Switches OpenCode's tab bar off, keeping every other key of its CLI config.
fn hide_tabs(
    login_env: &BTreeMap<String, String>,
    env: &mut BTreeMap<String, String>,
) -> Option<String> {
    const CONFIG_ENV: &str = "OPENCODE_CLI_CONFIG_CONTENT";
    // OpenCode reads this variable only when it is non-empty, unlike
    // OPENCODE_CONFIG_CONTENT, so `""` is absent here rather than malformed.
    let raw = login_env
        .get(CONFIG_ENV)
        .map(String::as_str)
        .filter(|raw| !raw.is_empty());
    let (mut config, warning) =
        parse_inline_config(CONFIG_ENV, raw, &[("/tabs", "an object", Value::is_object)]);
    let mut tabs = match config.remove("tabs") {
        Some(Value::Object(tabs)) => tabs,
        _ => serde_json::Map::new(),
    };
    tabs.insert("mode".to_owned(), Value::String("off".to_owned()));
    config.insert("tabs".to_owned(), Value::Object(tabs));
    env.insert(CONFIG_ENV.to_owned(), Value::Object(config).to_string());
    warning
}

type InlineConfigMember = (&'static str, &'static str, fn(&Value) -> bool);

/// Parses inline config against the nonempty list of member pointers this launch edits.
/// Content that fails is replaced by an empty object, with a warning naming the first
/// member that failed, or the first member listed when the content itself is malformed.
fn parse_inline_config(
    name: &str,
    raw: Option<&str>,
    members: &[InlineConfigMember],
) -> (serde_json::Map<String, Value>, Option<String>) {
    let Some(raw) = raw else {
        return (serde_json::Map::new(), None);
    };
    let parsed = crate::agents::jsonc::from_slice::<Value>(raw.as_bytes());
    let invalid = parsed.as_ref().ok().and_then(|config| {
        members
            .iter()
            .find(|(pointer, _, valid)| config.pointer(pointer).is_some_and(|value| !valid(value)))
    });
    let (pointer, shape, _) = invalid.unwrap_or(&members[0]);
    let member = pointer.trim_start_matches('/').replace('/', ".");
    let problem = match parsed {
        Ok(Value::Object(config)) if invalid.is_none() => return (config, None),
        Ok(Value::Object(_)) => format!("sets {member} to a value that is not {shape}"),
        Ok(_) => "is not a JSON object".to_owned(),
        Err(_) => "is not valid JSONC".to_owned(),
    };
    let warning = format!(
        "{name} {problem}; this launch ignored the configured value; \
         set it to a JSON object whose {member} is {shape}, or unset it"
    );
    (serde_json::Map::new(), Some(warning))
}

impl crate::agents::capabilities::HookCapability for OpencodeAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let parsed = payloads::parse_payload(payload);
        let state_snapshot = matches!(event_name, "client_idle" | "client_running");
        let ask_kind = match event_name {
            "permission_ask" => Some(AskKind::Permission),
            "question_ask" => Some(AskKind::Question),
            "session_idle" if parsed.plan_proposed == Some(true) => Some(AskKind::PlanApproval),
            _ => None,
        };
        let mut decoded = decode_catalog_hook(OPENCODE_HOOKS, event_name, ask_kind);
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        decoded.set_observed_context(self.observe_context(self.spec().kind, payload));
        let questions = if event_name == "question_ask" {
            super::question::questions(payload, super::question::PreviewPolicy::None)
                .unwrap_or_default()
                .into_iter()
                .take(4)
                .collect()
        } else {
            Vec::new()
        };
        let ask_detail = questions
            .first()
            .and_then(|question| question.question.lines().next())
            .map(ToOwned::to_owned)
            .filter(|detail| !detail.is_empty());
        decoded.set_ask(questions, ask_detail);
        decoded.set_native_answers(match event_name {
            "permission_replied" => parsed.reply.as_deref().and_then(|reply| {
                let reply = reply.trim().to_owned();
                (!reply.is_empty()).then_some(vec![AskAnswer {
                    question: None,
                    chosen: vec![reply],
                    note: None,
                }])
            }),
            "question_replied" => {
                let answers: Vec<_> = parsed
                    .answers
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .filter_map(|choices| {
                        let chosen: Vec<_> = choices
                            .iter()
                            .map(|choice| choice.trim().to_owned())
                            .filter(|choice| !choice.is_empty())
                            .collect();
                        (!chosen.is_empty()).then_some(AskAnswer {
                            question: None,
                            chosen,
                            note: None,
                        })
                    })
                    .collect();
                (!answers.is_empty()).then_some(answers)
            }
            "question_rejected" => Some(vec![AskAnswer {
                question: None,
                chosen: vec!["(rejected)".to_owned()],
                note: None,
            }]),
            _ => None,
        });
        let signal = match event_name {
            "session_created" | "client_idle" => Some(LifecycleSignal::Registered),
            "permission_ask" => Some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: parsed.title.clone(),
                native_key: None,
            }),
            "question_ask" => Some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Question,
                ask_id: None,
                detail: parsed.title.clone(),
                native_key: None,
            }),
            "chat_message" | "client_running" => {
                Some(LifecycleSignal::TurnStarted { turn_id: None })
            }
            "session_idle" if parsed.plan_proposed == Some(true) => {
                Some(LifecycleSignal::AwaitingInput {
                    kind: AskKind::PlanApproval,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                })
            }
            "session_idle" => Some(LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            }),
            "session_error" => Some(LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            }),
            "permission_replied" | "question_replied" | "question_rejected" => {
                Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    name: None,
                    native_key: None,
                    turn_id: None,
                })
            }
            "tool_after" => Some(LifecycleSignal::ToolUsed {
                mutates: self.spec().tool_mutates(payload),
                edits: self.spec().tool_edits_files(payload),
                name: optional_payload_string(payload, &["tool_name"]),
                native_key: None,
                turn_id: None,
            }),
            "session_compacting" => Some(LifecycleSignal::Compacting),
            "session_compacted" => Some(LifecycleSignal::CompactionEnded {
                auto: None,
                failed: payloads::errored(&parsed),
            }),
            "SubagentStart" => Some(LifecycleSignal::SubagentStarted),
            "SubagentStop" => Some(LifecycleSignal::SubagentStopped {
                errored: payloads::errored(&parsed),
            }),
            "session_ended" => Some(LifecycleSignal::Ended),
            _ => None,
        };
        let Some(signal) = signal else {
            return Ok(decoded);
        };
        let (agent_id, parent_agent_id) = if matches!(event_name, "SubagentStart" | "SubagentStop")
        {
            match resolve_subagent_identity(
                self.spec().kind,
                event_name,
                parsed.session_id.as_deref(),
                parsed.parent_session_id.as_deref(),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (Some(agent_id), Some(parent_agent_id)),
                SubagentIdentity::Quarantined => return Ok(decoded),
            }
        } else {
            (parsed.session_id.as_deref().map(AgentSessionId::from), None)
        };
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.on_screen = (parsed.on_screen == Some(true)).then_some(true);
        observation.state_snapshot = state_snapshot.then_some(true);
        observation.transcript_path = optional_payload_string(payload, &["transcript_path"])
            .or_else(|| database::newest_file().map(|path| path.to_string_lossy().into_owned()));
        observation.parent_agent_id = parent_agent_id;
        if !state_snapshot {
            observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
            observation.prompt = SanitizedPrompt::new(parsed.prompt.as_deref());
        }
        observation.launch.model = parsed.model.clone();
        observation.launch.effort = parsed.effort;
        observation.usage.context_window = parsed
            .context_window
            .or_else(|| context_window_for(parsed.model.as_deref()));
        observation.usage.total_tokens = parsed.total_tokens;
        observation.usage.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.usage.cache_write_input_tokens = parsed.cache_write_input_tokens;
        observation.usage.fresh_input_tokens = parsed.input_tokens;
        observation.usage.output_tokens = parsed.output_tokens;
        decoded.set_final_message(if matches!(event_name, "session_idle" | "session_error") {
            observation.agent_id.as_ref().and_then(|session_id| {
                let path = observation
                    .transcript_path
                    .as_deref()
                    .map(Path::new)
                    .filter(|path| path.is_file())
                    .map(Path::to_path_buf)
                    .or_else(|| database::files().into_iter().next())?;
                transcript::last_assistant_message(&path, session_id)
            })
        } else {
            None
        });
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for OpencodeAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&OpencodeManagedIntegration)
    }
}

fn install_blocker_at(path: &Path) -> Option<String> {
    let agent = crate::agents::find_definition("opencode")?;
    super::version::check_launch_version_floor(agent, path)
        .err()
        .map(|error| error.to_string())
}

struct OpencodeManagedIntegration;

impl super::ManagedIntegration for OpencodeManagedIntegration {
    fn install(
        &self,
        env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<super::HookInstallReport> {
        if let Some(reason) = self.install_blocker(env) {
            return Err(AgentErr::Install {
                agent: "opencode",
                reason,
            });
        }
        let files = OpencodeBundle::resolve()?.preflight()?;
        for file in &files {
            if file.candidate.is_empty() {
                std::fs::remove_file(&file.path).map_err(|source| AgentErr::InstallIo {
                    agent: "opencode",
                    path: file.path.clone(),
                    source,
                })?;
            } else if file.original.as_deref() != Some(file.candidate.as_str()) {
                crate::agents::provider_file::write_bytes(&file.path, file.candidate.as_bytes())?;
            }
        }
        Ok(super::HookInstallReport {
            agent: "opencode",
            files: files
                .into_iter()
                .map(|file| HookInstallFileReport {
                    path: file.path,
                    existed: file.existed,
                })
                .collect(),
            installed_events: opencode_event_names(),
        })
    }

    fn preview(
        &self,
        env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<super::HookInstallPreview> {
        if let Some(reason) = self.install_blocker(env) {
            return Err(AgentErr::Install {
                agent: "opencode",
                reason,
            });
        }
        Ok(super::HookInstallPreview {
            agent: "opencode",
            files: OpencodeBundle::resolve()?.preflight()?,
            planned_events: opencode_event_names(),
            status_line_change: None,
            subagent_status_line_change: None,
        })
    }

    fn uninstall(&self, _env: &BTreeMap<String, String>) -> Result<super::HookUninstallReport> {
        let bundle = OpencodeBundle::resolve()?;
        let mut files = Vec::new();
        for file in bundle.inspect()? {
            if !file
                .original
                .as_deref()
                .is_some_and(opencode_managed_source)
            {
                continue;
            }
            std::fs::remove_file(&file.path).map_err(|source| AgentErr::InstallIo {
                agent: "opencode",
                path: file.path.clone(),
                source,
            })?;
            files.push(HookInstallFileReport {
                path: file.path,
                existed: true,
            });
        }
        match std::fs::remove_dir(&bundle.directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(source) => {
                return Err(AgentErr::InstallIo {
                    agent: "opencode",
                    path: bundle.directory,
                    source,
                });
            }
        }
        Ok(super::HookUninstallReport {
            agent: "opencode",
            removed_events: if files.is_empty() {
                Vec::new()
            } else {
                opencode_event_names()
            },
            files,
        })
    }

    fn installed(&self, _env: &BTreeMap<String, String>) -> bool {
        OpencodeBundle::resolve().is_ok_and(|bundle| bundle.installed())
    }

    fn upgrade_available(
        &self,
        _env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> bool {
        OpencodeBundle::resolve().is_ok_and(|bundle| {
            bundle.managed_artifacts_present()
                && !bundle.inspect().is_ok_and(|files| {
                    files.len() == 2
                        && files
                            .iter()
                            .all(|file| file.original.as_deref() == Some(file.candidate.as_str()))
                })
        })
    }

    fn managed_artifacts_present(&self, _env: &BTreeMap<String, String>) -> bool {
        OpencodeBundle::resolve().is_ok_and(|bundle| bundle.managed_artifacts_present())
    }

    fn wiring_input_paths(
        &self,
        _descriptor: &AgentSpec,
        _env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        OpencodeBundle::resolve()
            .map(|bundle| bundle.wiring_input_paths())
            .unwrap_or_default()
    }

    fn install_blocker(&self, _login_env: &BTreeMap<String, String>) -> Option<String> {
        install_blocker_at(&crate::agents::locate_binary(&OPENCODE_DESCRIPTOR)?)
    }
}

struct OpencodeBundle {
    directory: PathBuf,
    legacy: Option<PathBuf>,
}

impl OpencodeBundle {
    fn resolve() -> Result<Self> {
        if let Some(raw) =
            std::env::var_os("RIMZ_OPENCODE_PLUGIN").filter(|value| !value.is_empty())
        {
            return Ok(Self {
                directory: PathBuf::from(raw),
                legacy: None,
            });
        }
        let home = opencode_config_home(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
        .ok_or_else(|| AgentErr::Install {
            agent: "opencode",
            reason: "$HOME is not set; cannot resolve ~/.config/opencode/plugins/rimz".into(),
        })?;
        Ok(Self {
            directory: home.join("plugins/rimz"),
            legacy: Some(home.join("plugin/rimz.ts")),
        })
    }

    fn check_directory(&self) -> Result<()> {
        match std::fs::symlink_metadata(&self.directory) {
            Ok(metadata) if !metadata.is_dir() => Err(AgentErr::Install {
                agent: "opencode",
                reason: format!(
                    "{} must be a real directory, not a symlink or file",
                    self.directory.display()
                ),
            }),
            Ok(_) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AgentErr::InstallIo {
                agent: "opencode",
                path: self.directory.clone(),
                source,
            }),
        }
    }

    fn inspect(&self) -> Result<Vec<HookInstallFilePreview>> {
        self.check_directory()?;
        let mut files = Vec::new();
        for (name, source) in [("index.ts", PLUGIN_SOURCE), ("tui.ts", TUI_SOURCE)] {
            let path = self.directory.join(name);
            let original = read_opencode_source(&path)?;
            files.push(HookInstallFilePreview {
                path,
                existed: original.is_some(),
                original,
                candidate: source.into(),
            });
        }
        if let Some(path) = &self.legacy
            && let Some(original) = read_opencode_source(path)?
        {
            files.push(HookInstallFilePreview {
                path: path.clone(),
                original: Some(original),
                existed: true,
                candidate: String::new(),
            });
        }
        Ok(files)
    }

    fn preflight(&self) -> Result<Vec<HookInstallFilePreview>> {
        let files = self.inspect()?;
        for file in &files {
            if file
                .original
                .as_deref()
                .is_some_and(|text| !opencode_managed_source(text))
            {
                return Err(AgentErr::Install {
                    agent: "opencode",
                    reason: format!(
                        "refusing an unmarked user plugin at {}; move it aside or remove it to let RimZ manage this file",
                        file.path.display()
                    ),
                });
            }
        }
        Ok(files)
    }

    fn installed(&self) -> bool {
        self.inspect().is_ok_and(|files| {
            files.len() == 2
                && files.iter().all(|file| {
                    file.original
                        .as_deref()
                        .is_some_and(opencode_managed_source)
                })
        })
    }

    fn managed_artifacts_present(&self) -> bool {
        let bundle_owned = self.check_directory().is_ok()
            && ["index.ts", "tui.ts"].iter().any(|name| {
                read_opencode_source(&self.directory.join(name))
                    .is_ok_and(|text| text.as_deref().is_some_and(opencode_managed_source))
            });
        bundle_owned
            || self.legacy.as_ref().is_some_and(|path| {
                read_opencode_source(path)
                    .is_ok_and(|text| text.as_deref().is_some_and(opencode_managed_source))
            })
    }

    fn wiring_input_paths(self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.directory.join("index.ts"),
            self.directory.join("tui.ts"),
        ];
        paths.extend(self.legacy);
        paths.push(self.directory);
        paths
    }
}

fn read_opencode_source(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(AgentErr::Install {
                agent: "opencode",
                reason: format!(
                    "{} must be a real file, not a symlink or directory; move it aside",
                    path.display()
                ),
            });
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(AgentErr::InstallIo {
                agent: "opencode",
                path: path.to_owned(),
                source,
            });
        }
    }
    read_optional_file("opencode", path)
}

fn opencode_managed_source(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
}

fn opencode_event_names() -> Vec<String> {
    OPENCODE_HOOKS
        .iter()
        .map(|hook| hook.event.to_owned())
        .collect()
}

impl crate::agents::capabilities::TranscriptCapability for OpencodeAdapter {
    fn read_transcript_messages(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> std::io::Result<Vec<crate::agents::TranscriptMessage>> {
        transcript::read_messages(path, session_id)
    }

    fn transcript_position(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> Option<crate::agents::TranscriptPosition> {
        transcript::position(path, session_id)
    }

    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
        position: crate::agents::TranscriptPosition,
    ) -> Option<crate::agents::TranscriptPage> {
        transcript::read_assistant_page(path, session_id, position)
    }
}

impl crate::agents::capabilities::ContextCapability for OpencodeAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        let parsed = payloads::parse_payload(payload);
        if parsed.session_name.is_none()
            && parsed.model_display_name.is_none()
            && parsed.agent_version.is_none()
        {
            return None;
        }
        let context = AgentContext {
            session_name: parsed.session_name,
            model_display_name: parsed.model_display_name,
            agent_version: parsed.agent_version,
            ..AgentContext::new(source, Timestamp::now())
        };
        super::ContextObservation::new(parsed.session_id?, context)
    }
}

impl crate::agents::capabilities::AccountCapability for OpencodeAdapter {
    fn has_quota_surface(&self, sub_provider: &str) -> bool {
        crate::agents::delegated_account::Adapter::OpenCode.has_quota_surface(sub_provider)
    }

    fn probe_account(
        &self,
        _login_env: &BTreeMap<String, String>,
    ) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(
        &self,
        _login_env: &BTreeMap<String, String>,
    ) -> crate::agents::AccountUsageProbe {
        account::probe_usage(None)
    }

    fn probe_account_usage_for(
        &self,
        _login_env: &BTreeMap<String, String>,
        sub_provider: Option<&str>,
    ) -> crate::agents::AccountUsageProbe {
        account::probe_usage(sub_provider)
    }
}

impl crate::agents::capabilities::SpendingCapability for OpencodeAdapter {
    fn transcript_stat(&self, path: &Path) -> Option<crate::agents::TranscriptStat> {
        database::logical_stat(path)
    }

    fn spending_sources(
        &self,
        _login_env: &BTreeMap<String, String>,
    ) -> Vec<crate::agents::spending::SpendingSource> {
        database::files()
            .into_iter()
            .map(crate::agents::spending::SpendingSource::exact)
            .collect()
    }

    fn session_transcript(
        &self,
        _session_id: &str,
        prior_path: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        self.transcript_files(login_env).into_iter().next()
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse(path, resume, prices)
    }
}

/// Offline fallback window when the plugin's catalog-resolved
/// [`context_window`](payloads::OpencodeHookPayload::context_window) is absent —
/// Claude-family only. Every other model resolves through the plugin's model
/// catalog ([`index.ts`](./index.ts) or [`tui.ts`](./tui.ts)); a bare id here stays unknown.
fn context_window_for(model: Option<&str>) -> Option<u64> {
    let model = model?.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    if model.contains("[1m]") || (model.contains("1m") && model.contains("claude")) {
        return Some(1_000_000);
    }
    if model.starts_with("claude-") || model.contains("/claude-") {
        return Some(200_000);
    }
    None
}

fn opencode_config_home(
    configured: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    configured
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .map(|home| home.join("opencode"))
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for OpencodeAdapter {}
impl crate::agents::capabilities::SessionCapability for OpencodeAdapter {}

#[cfg(test)]
mod tests;
