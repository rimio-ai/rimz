//! Antigravity CLI launch, hook, statusline, local-session, transcript, and account adapter.
//! The plugin install, the kept `statusLine` keys, and the `agy -p /usage` envelope were
//! observed on agy 1.5.12; the launch agent file (`agent_file`) on agy 1.5.13.
//!
//! RimZ installs only hooks with documented observer-neutral output. The
//! policy-changing `PreToolUse` decision channel stays untouched; disjoint
//! `PostToolUse` matchers recover tool phase after execution instead.

mod agent_file;
mod install;
mod payloads;
mod permissions;
mod session;
mod spend;
mod statusline;
mod usage;
// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for AntigravityAdapter {}

#[cfg(test)]
mod tests;

use crate::agents::capabilities::*;
use std::collections::BTreeMap;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

#[cfg(test)]
use super::AgentHookClass;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, LocalSessionDiscovery, PlanLabel, RemoteControlCapability,
    SamePaneSessionPolicy, ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::{LifecycleSignal, ToolEffect};
use super::{
    AgentLifecycleObservation, HookOutput, HookRouting, LocalSessionObservation, Result,
    SanitizedPrompt, SpawnedSubagent, SubagentCorrelation, SubagentCorrelationInput,
    SubagentIdentity, SubagentSpawnInput, TranscriptMessage, non_empty_trimmed,
    resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::agents::PermissionMode;
use session::{PromptEvidence, PromptRead};

const HOOK_TIMEOUT_SECS: u64 = 5;
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source antigravity";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source antigravity";
const STATUS_LINE_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity";
const COMPACT_COMMAND: &str = "/compact";
/// agy names a plugin's server `<plugin>_<server>`: the rimz plugin's `tools` entry
/// (`install::plugin_files`).
const MCP_SERVER_NAME: &str = "rimz_tools";
const ADD_DIR_FLAG: &str = "--add-dir";
const SANDBOX_FLAG: &str = "--sandbox";
const SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
/// The settings document an ask launch passes: an empty `agentMode` starts agy in its Default
/// mode, which asks before a write or a command, and wins over `--mode` in either order without
/// being saved (agy 1.5.14). With no mode flag agy 1.5.7 and later start in Auto instead.
/// `fold_permission_settings` folds it into the launch's one `--settings`.
const ASK_SETTINGS: [&str; 2] = ["--settings", r#"{"agentMode":""}"#];
struct AntigravityHook {
    pub(super) hook: HookEventSpec,
    pub(super) config_event: &'static str,
    pub(super) config_matcher: Option<&'static str>,
    pub(super) command: &'static str,
}

const ANTIGRAVITY_HOOKS: [AntigravityHook; 6] = [
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PreInvocation",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","invocationNum":0}"#
        )
        .progress(),
        config_event: "PreInvocation",
        config_matcher: None,
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PreInvocation",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PostToolUse:edit",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        )
        .progress(),
        config_event: "PostToolUse",
        config_matcher: Some(
            "^(write_to_file|replace_file_content|multi_replace_file_content|write_file|edit_file)$",
        ),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:edit",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PostToolUse:mutating",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        )
        .progress(),
        config_event: "PostToolUse",
        config_matcher: Some("^(run_command|run_workflow|call_mcp_tool)$"),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:mutating",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PostToolUse:observed",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        )
        .progress(),
        config_event: "PostToolUse",
        config_matcher: Some(
            "^(view_file|read_file|list_dir|find_by_name|grep_search|search_web|read_url_content|manage_task|schedule|list_permissions|ask_permission|invoke_subagent|define_subagent|send_message|manage_subagents|ask_question|generate_image|manage_inbox|read_resource|browser_.*)$",
        ),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:observed",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PostInvocation",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        )
        .progress(),
        config_event: "PostInvocation",
        config_matcher: None,
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostInvocation",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "Stop",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","fullyIdle":true,"terminationReason":"model_stop"}"#
        )
        .progress(),
        config_event: "Stop",
        config_matcher: None,
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event Stop",
    },
];

const fn antigravity_event_names<const N: usize>(
    hooks: &[AntigravityHook; N],
) -> [&'static str; N] {
    let mut names = [""; N];
    let mut index = 0;
    while index < N {
        names[index] = hooks[index].hook.event;
        index += 1;
    }
    names
}

const ANTIGRAVITY_EVENT_NAMES: [&str; 6] = antigravity_event_names(&ANTIGRAVITY_HOOKS);

static ANTIGRAVITY_DESCRIPTOR: AgentSpec = AgentSpec {
    tool_rules: crate::agents::skills::ToolRules::Settings {
        render: permissions::render,
        translate: permissions::translate,
    },
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    kind: "antigravity",
    aliases: &["agy"],
    display_name: "Antigravity",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x42, 0x85, 0xf4),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    plan_capacities: None,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
            "write_file",
            "edit_file",
            "run_command",
            "run_workflow",
            "call_mcp_tool",
        ],
        editing: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
            "write_file",
            "edit_file",
        ],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        permission_before_auto_review: false,
        transcript_tail_context: true,
        registers_lazily: true,
        reports_start_on_resume: false,
        local_session_discovery: LocalSessionDiscovery::FullScan,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: SamePaneSessionPolicy::FollowLatest,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: ANTIGRAVITY_COVERAGE,
    user_coverage: ANTIGRAVITY_USER_COVERAGE,
    lifecycle_hooks: ANTIGRAVITY_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["agy"],
    bin_names: &["agy"],
    bin_identity: None,
    extra_bin_dirs: &[".local/bin"],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        definitions: crate::agents::definition::DefinitionSpec {
            models: MODELS,
            prefixes: &["gemini-"],
            foreign_prefixes: &[("claude-", "high")],
            tools: crate::agents::definition::DefinitionTools::RequiredFile(
                agent_file::render_definition_tools,
            ),
            ..crate::agents::definition::DefinitionSpec::EMPTY
        },
        program: Some("agy"),
        fixed_args: &[],
        prompt: super::PromptStyle::Flag("--prompt-interactive"),
        resume: None,
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &ASK_SETTINGS,
            auto: &["--mode", "auto"],
            yolo: &[SKIP_PERMISSIONS_FLAG],
            plan: &["--mode", "plan"],
            ..super::LaunchPermissionArgs::EMPTY
        },
        max_turn_flag: None,
        interrupt_key: Some(crate::pane::keys::NamedKey::Escape),
        interrupt_returns_queued_text: true,
        steer_key: Some(crate::pane::keys::NamedKey::AltM),
        compact_command: Some(super::CompactCommand {
            command: COMPACT_COMMAND,
            instruction: super::CompactInstruction::Unsupported,
        }),
        presets: super::PresetMatchers {
            code_mode: super::CodeModeSwitch::Unsupported,
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::ValueFlag {
                flags: &["--effort"],
                values: &["low", "medium", "high", "xhigh"],
            }),
            system_prompt_file: Some(super::StaticPresetMatcher::AgentFile(
                agent_file::AGENT_FLAG,
            )),
            ..super::PresetMatchers::EMPTY
        },
        ..super::LaunchSpec::EMPTY
    },
};

/// The Gemini models agy runs, each launched by base id with `--effort`, never by an
/// effort-suffixed id: agy knows no short name, and refuses a suffixed id beside a differing
/// `--effort`. Each defaults to `high`, the highest level every model takes, and sets no
/// kind-wide default, so an uncataloged or suffixed id launches as written. Neon and hydrogen
/// take low to xhigh; carbon, argon, and flash low to high (agy 1.5.13). The Orchestrator ids
/// take no effort and no short name: agy 1.5.14 runs Gemini 4 Neon instead when `--effort`
/// comes with one, so a launch of one never carries it.
const MODELS: &[crate::agents::definition::DefinitionModel] = &[
    gemini("hydrogen", "gemini-4-hydrogen", "xhigh"),
    gemini("neon", "gemini-4-neon", "xhigh"),
    gemini("carbon", "gemini-4-carbon", "high"),
    gemini("argon", "gemini-4-argon", "high"),
    gemini("flash", "gemini-3.8-flash", "high"),
    orchestrator("gemini-pro-orchestrator-neon"),
    orchestrator("gemini-pro-orchestrator"),
    orchestrator("gemini-pro-orchestrator-argon"),
];

const fn gemini(
    name: &'static str,
    id: &'static str,
    max_effort: &'static str,
) -> crate::agents::definition::DefinitionModel {
    crate::agents::definition::DefinitionModel {
        name,
        id,
        effort: Some("high"),
        max_effort: Some(max_effort),
        takes_effort: true,
    }
}

const fn orchestrator(id: &'static str) -> crate::agents::definition::DefinitionModel {
    crate::agents::definition::DefinitionModel {
        name: id,
        id,
        effort: None,
        max_effort: None,
        takes_effort: false,
    }
}

const ANTIGRAVITY_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "neutral PreInvocation, PostToolUse, and Stop hooks",
    },
    permission: ConcernCoverage::Partial {
        via: "statusline tool_confirmation_pending routes the card to the native pane",
        gap: "PreToolUse stays unregistered, since an empty or failed reply denies the tool (a late reply is uncaptured), so there is no durable ask or permission detail",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "artifact status enums and review transitions remain uncaptured",
    },
    user_question: ConcernCoverage::Partial {
        via: "validated local transcripts project ask_question records to a native waiting card",
        gap: "there is no durable RimZ ask or out-of-band answer API",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no out-of-band answer API or verified native-key planner",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreInvocation and Stop hooks whose lastUserInput is a bare /compact",
    },
    subagents: ConcernCoverage::Partial {
        via: "child hooks joined to ordered invoke_subagent parent transcript results",
        gap: "the parent-result transcript shape is live-verified rather than a documented stable wire contract",
    },
    launch_reminders: ConcernCoverage::Wired {
        via: "a fresh launch's agent file in RimZ's agy plugin, selected by --agent",
    },
    background_parking: ConcernCoverage::Wired {
        via: "Stop.fullyIdle parks a clean foreground stop while background work remains",
    },
    background_shells: ConcernCoverage::Unsupported {
        reason: "background shells are not mapped",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "Antigravity publishes no session-end event",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "native Stop wakeup + pane liveness",
        gap: "Antigravity publishes no separate idle-notification event",
    },
    context_usage: ConcernCoverage::Wired {
        via: "custom statusline context_window payload",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "transcript_full token rows priced at list price by exact model as the file grows",
    },
    rich_context: ConcernCoverage::Wired {
        via: "custom statusline context plus agy -p /usage quota windows",
    },
    hook_install: ConcernCoverage::Wired {
        via: "RimZ-owned agy plugin directory plus reversible statusLine wrap",
    },
    account_spend: ConcernCoverage::Partial {
        via: "transcript_full token rows priced at list price by exact model",
        gap: "quota is work-metered, so dollars are a list-price estimate",
    },
    tool_stats: ConcernCoverage::Partial {
        via: "PostToolUse toolCall names on matched tools",
        gap: "releases that omit toolCall send no name, and transcript history is not counted",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "agy offers daemon and session remote control, but RimZ wires neither",
    },
};

const ANTIGRAVITY_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card tracks every turn from session start to session end",
    },
    live: CapabilityLevel::Partial {
        shows: "context fill, token composition, and the session's priced dollar total",
        limit: "the dollar total is a list-price estimate, not what the plan bills",
    },
    history: CapabilityLevel::Partial {
        shows: "past sessions read end to end, with per-turn tokens and dollars",
        limit: "dollars are a list-price estimate, not what the plan bills",
    },
    account: CapabilityLevel::Full {
        note: "Gemini 5h and weekly quota windows with fill and reset, plus plan tier",
    },
    ask: CapabilityLevel::Partial {
        shows: "the card raises Waiting and routes you to the pane",
        limit: "the question stays in Antigravity's own UI, so rimz asks stays empty",
    },
    subagents: CapabilityLevel::Partial {
        shows: "children nest under the parent card with name, task, and tokens",
        limit: "they often land only once the parent turn ends",
    },
};

const ANTIGRAVITY_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Derived {
        via: "first PreInvocation create-on-miss + validated local conversation discovery",
        gap: "SessionStart stays unregistered: on agy 1.5.14 it fires on no resume and carries no source",
    },
    turn_started: HookCoverage::Native {
        event: "PreInvocation",
    },
    turn_ended: HookCoverage::Native { event: "Stop" },
    tool_used: HookCoverage::Native {
        event: "PostToolUse:edit",
    },
    awaiting_input: HookCoverage::Derived {
        via: "statusline tool_confirmation_pending marker + pulled transcript questions",
        gap: "read-only attention projection, not a durable AwaitingInput lifecycle signal",
    },
    subagent_started: HookCoverage::Derived {
        via: "child PreInvocation joined to its parent's invoke_subagent transcript result",
        gap: "the parent-result transcript shape is live-verified rather than documented",
    },
    subagent_stopped: HookCoverage::Derived {
        via: "child Stop joined to its parent's invoke_subagent transcript result",
        gap: "the parent-result transcript shape is live-verified rather than documented",
    },
    compacting: HookCoverage::Native {
        event: "PreInvocation",
    },
    compaction_ended: HookCoverage::Native { event: "Stop" },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "provider callbacks stop with the mux session",
    },
};

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct AntigravityAdapter;

impl crate::agents::capabilities::CoreCapability for AntigravityAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &ANTIGRAVITY_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        let mut classification: Vec<_> = ANTIGRAVITY_HOOKS
            .iter()
            .map(|entry| super::hook_types::classification_sample(&entry.hook))
            .collect();
        // A bare `/compact` brackets its compaction on the same two events (probe link 5, P1).
        for (event, payload) in [
            (
                "PreInvocation",
                r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","invocationNum":0,"lastUserInput":"/compact"}"#,
            ),
            (
                "Stop",
                r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","fullyIdle":true,"terminationReason":"TERMINAL_CUSTOM_HOOK","lastUserInput":"/compact"}"#,
            ),
        ] {
            classification.push(super::ClassificationSample::new(
                event,
                serde_json::from_str(payload).expect("valid compaction sample"),
                AgentHookClass::Lifecycle,
                None,
            ));
        }
        super::AdapterConformance {
            classification,
            spend: Some(super::SpendFixture {
                session_id: "11111111-1111-4111-8111-111111111111",
                file_name: "brain/11111111-1111-4111-8111-111111111111/.system_generated/logs/transcript_full.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-07-13T23:23:09Z","model":"gemini-3-flash-preview","input_tokens":100,"cache_read_tokens":40,"output_tokens":20}"#,
                ),
            }),
            context_model: vec![
                super::ContextModelFixture {
                    effort: "xhigh",
                    producer: super::ContextModelProducer::Push {
                        source: "antigravity",
                        payload: serde_json::json!({
                            "conversation_id": "11111111-1111-4111-8111-111111111111",
                            "model": {
                                "id": "Gemini 4 Hydrogen (XHigh)",
                                "display_name": "Gemini 4 Hydrogen (XHigh)"
                            }
                        }),
                    },
                },
                super::ContextModelFixture {
                    effort: "xhigh",
                    producer: super::ContextModelProducer::Hook {
                        event_name: "PreInvocation",
                        payload: serde_json::json!({
                            "conversationId": "11111111-1111-4111-8111-111111111111",
                            "workspacePaths": ["/workspace/project"],
                            "invocationNum": 0,
                            "modelName": "gemini-4-hydrogen-xhigh"
                        }),
                    },
                },
            ],
            local_session: Some(session::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for AntigravityAdapter {
    fn payload_hook_reply(&self, event_name: &str, _payload: &Value) -> super::HookReply {
        match event_name {
            "Stop" => super::HookReply::Json(serde_json::json!({ "decision": "" })),
            event if ANTIGRAVITY_EVENT_NAMES.contains(&event) => {
                super::HookReply::Json(serde_json::json!({}))
            }
            _ => super::HookReply::Silent,
        }
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let home = std::env::var("HOME").ok();
        let login_env = home.into_iter().map(|home| ("HOME".to_owned(), home));
        self.decode_hook_in(event_name, payload, &login_env.collect())
    }

    fn decode_hook_in(
        &self,
        event_name: &str,
        payload: &Value,
        login_env: &BTreeMap<String, String>,
    ) -> Result<HookOutput> {
        let payload = &*session::payload_on_host(payload, login_env);
        let home = session::login_home(login_env);
        let mut decoded = decode_catalog_hook(
            ANTIGRAVITY_HOOKS.iter().map(|entry| &entry.hook),
            event_name,
            None,
        );
        decoded.set_reply(self.payload_hook_reply(event_name, payload));
        let fields = decode_lifecycle_fields(event_name, payload, |path, id, read| {
            home.as_deref().map_or(PromptEvidence::Unread, |home| {
                session::read_prompt_under(home, path, id, read)
            })
        });
        decoded.set_routing(
            HookRouting::session(fields.agent_id.map(Into::into))
                .with_worktree(fields.worktree_path),
        );
        if let Some(observation) = fields.lifecycle.as_ref()
            && matches!(observation.signal, LifecycleSignal::TurnEnded { .. })
        {
            decoded.set_final_message(
                observation
                    .transcript_path
                    .as_deref()
                    .and_then(|path| session::last_assistant_message(Path::new(path))),
            );
        }
        if let Some(observation) = fields.lifecycle {
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }

    fn correlate_subagent(
        &self,
        input: SubagentCorrelationInput<'_>,
    ) -> Option<SubagentCorrelation> {
        let (child_id, parent_id) = match resolve_subagent_identity(
            self.spec().kind,
            "transcript_correlation",
            Some(input.child_agent_id.as_str()),
            Some(input.parent_agent_id.as_str()),
            &Value::Null,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => (agent_id, parent_agent_id),
            SubagentIdentity::Quarantined => return None,
        };
        let parent_transcript = input
            .parent_transcript_path
            .map(Path::to_path_buf)
            .or_else(|| session::transcript_for_session(parent_id.as_str(), input.login_env))?;
        let correlated = session::correlate_subagent(
            &parent_transcript,
            parent_id.as_str(),
            input.parent_workspace?,
            child_id.as_str(),
            input.child_workspace?,
            input.login_env,
        )?;
        let role = non_empty_trimmed(&correlated.role);
        let prompt = sanitize_user_prompt(Some(&correlated.prompt));
        Some(SubagentCorrelation {
            agent_name: non_empty_trimmed(&correlated.type_name),
            role: role.clone(),
            task: role.or_else(|| prompt.clone()),
            prompt,
            model: None,
        })
    }

    fn spawned_subagents(&self, input: SubagentSpawnInput<'_>) -> Vec<SpawnedSubagent> {
        let Some(parent_workspace) = input.parent_workspace else {
            return Vec::new();
        };
        let Some(parent_transcript) =
            input
                .parent_transcript_path
                .map(Path::to_path_buf)
                .or_else(|| {
                    session::transcript_for_session(input.parent_agent_id.as_str(), input.login_env)
                })
        else {
            return Vec::new();
        };
        session::spawned_subagents(
            &parent_transcript,
            input.parent_agent_id.as_str(),
            parent_workspace,
            input.login_env,
        )
    }
}

impl crate::agents::capabilities::InstallationCapability for AntigravityAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

impl crate::agents::capabilities::LaunchCapability for AntigravityAdapter {
    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::AgentFile)
    }

    /// A catalog short name launches its base id; agy has no catalog to read.
    fn resolve_model_alias(
        &self,
        request: ModelAliasRequest<'_>,
        _source: Option<&mut dyn ModelCatalogSource>,
    ) -> Option<ModelAliasResolution> {
        let entry = MODELS.iter().find(|entry| entry.name == request.alias)?;
        Some(ModelAliasResolution {
            id: entry.id.to_owned(),
            rung: ModelAliasRung::Baked,
            warnings: Vec::new(),
            movement: None,
            refresh_failure: None,
        })
    }

    fn agent_file(
        &self,
        input: &crate::agents::AgentFileInput<'_>,
    ) -> Option<std::result::Result<crate::agents::AgentFile, crate::agents::AgentFileRefusal>>
    {
        Some(agent_file::plan(input))
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        session::login_home(env)
    }

    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &[session::HOME_ENV]
    }

    /// agy reads no variable or flag that moves its data directory (1.5.15), so a named
    /// account is a directory mounted over it, with `HOME` left as the user's.
    fn mounted_home_target(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        session::view_home(env)
    }

    /// A link into the default directory would resolve inside the mount, to the account itself.
    fn account_history_shareable(&self) -> bool {
        false
    }

    /// `--add-dir` opens a workspace: agy loads its `AGENTS.md`, `.agents/rules`, and
    /// `.agents/skills` and runs its `.agents/hooks.json`, with no trust prompt (agy 1.5.14).
    fn added_dirs(&self) -> AddedDirs {
        AddedDirs::Workspace
    }

    fn allow_added_dirs_args(
        &self,
        dirs: &[String],
        _paths: (&Path, &Path),
        extra_args: &mut Vec<String>,
        _artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        let present: Vec<String> =
            crate::agents::PresetArgMatcher::Flag(vec![ADD_DIR_FLAG.to_owned()])
                .occurrences(extra_args)
                .into_iter()
                .map(|entry| entry.value)
                .collect();
        for dir in dirs.iter().filter(|dir| !present.contains(dir)) {
            extra_args.extend([ADD_DIR_FLAG.to_owned(), dir.clone()]);
        }
        Ok(())
    }

    fn fold_permission_settings(
        &self,
        paths: (&Path, &Path),
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        permissions::pin_ask_mode(&ASK_SETTINGS, paths, extra_args, artifact)
    }

    /// `--sandbox` confines agy's terminal commands to the workspace and added dirs, with no
    /// network (agy 1.5.14).
    fn has_native_sandbox(&self) -> bool {
        true
    }

    /// The skip flag runs commands outside the sandbox even beside `--sandbox` (agy 1.5.14).
    fn native_sandbox_overridden(&self, argv: &[String]) -> bool {
        argv.iter().any(|arg| arg == SKIP_PERMISSIONS_FLAG)
    }

    fn configure_room_permissions(
        &self,
        extra_args: &mut Vec<String>,
        _login_env: &BTreeMap<String, String>,
        _paths: (&Path, Option<&Path>),
        _roots: &[PathBuf],
        isolation: crate::config::Isolation,
    ) {
        if isolation == crate::config::Isolation::Sandbox
            && !extra_args.iter().any(|arg| arg == SANDBOX_FLAG)
        {
            extra_args.push(SANDBOX_FLAG.to_owned());
        }
    }

    /// The list rides the agent file (`agent_file`) and the server the rimz plugin's
    /// `mcp_config.json` (`install`), never argv, so `mcp_server_args` keeps its default.
    fn takes_mcp_servers(&self) -> bool {
        true
    }

    fn serves_mcp_from_launch_env(&self) -> bool {
        true
    }

    fn mcp_server_name(&self) -> &'static str {
        MCP_SERVER_NAME
    }

    /// The global root agy's own built-in skills name. agy also reads
    /// `~/.gemini/antigravity-cli/skills` (above this root on a duplicate name), `~/.gemini/skills`
    /// (below it, agy 1.5.14), a plugin's `skills/`, and the workspace `.agents/skills`, which
    /// the scoped view leaves native.
    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        env.get("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| Path::new(home).join(".gemini/config/skills"))
    }

    fn manual_skill(&self) -> ManualSkill {
        ManualSkill::Frontmatter
    }

    fn probe_version(&self) -> Option<String> {
        None
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        session::valid_conversation_id(session_id).then(|| {
            vec![
                "agy".to_owned(),
                "--conversation".to_owned(),
                session_id.to_owned(),
            ]
        })
    }
}

impl crate::agents::capabilities::SessionCapability for AntigravityAdapter {
    fn discover_local_sessions(
        &self,
        workspaces: &[&Path],
        login_env: &BTreeMap<String, String>,
    ) -> Vec<LocalSessionObservation> {
        session::discover(workspaces, login_env)
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }
}

impl crate::agents::capabilities::TranscriptCapability for AntigravityAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for AntigravityAdapter {
    fn prompt_cache_ttl(
        &self,
        _sign_in: Option<crate::agents::context::SignIn>,
    ) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(60 * 60))
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        if !payload.is_object() {
            return None;
        }
        let parsed = statusline::StatuslinePayload::deserialize(payload).ok()?;
        let agent_id = parsed.conversation_id.clone()?;
        let at_rest = parsed.at_rest();
        let mut observation = super::ContextObservation::new(
            agent_id,
            parsed.into_context(source, Timestamp::now()),
        )?;
        observation.provider_at_rest = at_rest;
        Some(observation)
    }

    fn local_context_refresh(
        &self,
        _trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        // A hook may name `transcript.jsonl`, which repeats the rows spend reads.
        let is_source = |path: &Path| {
            path.file_name()
                .is_some_and(|name| name == spend::SOURCE_TRANSCRIPT)
        };
        let hint = ctx
            .current_transcript_path
            .or(ctx.prior_transcript_path)
            .map(Path::new)
            .filter(|path| is_source(path));
        let path = crate::agents::capabilities::SpendingCapability::session_transcript(
            self,
            ctx.agent_id,
            hint,
            ctx.login_env,
        )
        .filter(|path| is_source(path))?;
        spend::refresh(&path, ctx)
    }
}

impl crate::agents::capabilities::AccountCapability for AntigravityAdapter {
    fn probe_account(&self, login_env: &BTreeMap<String, String>) -> super::account::AccountProbe {
        let provider_home = self.config_home(login_env);
        usage::read(provider_home.clone(), login_env).account(provider_home.as_deref())
    }

    fn foreign_login(&self, login_env: &BTreeMap<String, String>) -> Option<String> {
        let (account, _) = session::mounted_view(login_env)?;
        if usage::holds_credential(&account) {
            return None;
        }
        usage::read(self.config_home(login_env), login_env)
            .launch_refusal()
            .map(str::to_owned)
    }

    fn probe_account_usage(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> super::AccountUsageProbe {
        usage::read(self.config_home(login_env), login_env).usage()
    }
}

impl crate::agents::capabilities::SpendingCapability for AntigravityAdapter {
    fn spending_sources(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<crate::agents::spending::SpendingSource> {
        self.config_home(login_env)
            .map(|home| spend::sources(&home))
            .unwrap_or_default()
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse(path, resume, prices)
    }

    fn session_transcript(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        let home = session::login_home(login_env)?;
        if let Some(path) =
            prior_path.filter(|path| session::valid_transcript_under(&home, path, session_id))
        {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session_under(&home, session_id)
    }
}

/// The effort levels agy appends to a model slug (`gemini-4-neon-high`).
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A model slug split into its base id and the effort agy appends to it. Hook `modelName`
/// and transcript `model` carry the suffixed form; RimZ keeps only the base id.
fn split_model_effort(model: &str) -> (&str, Option<&'static str>) {
    EFFORT_LEVELS
        .iter()
        .find_map(|effort| {
            let base = model.strip_suffix(effort)?.strip_suffix('-')?;
            (!base.is_empty()).then_some((base, Some(*effort)))
        })
        .unwrap_or((model, None))
}

#[derive(Default)]
struct DecodedLifecycleFields {
    agent_id: Option<String>,
    worktree_path: Option<String>,
    lifecycle: Option<AgentLifecycleObservation>,
}

fn decode_lifecycle_fields(
    event_name: &str,
    payload: &Value,
    prompt_reader: impl FnOnce(&Path, &str, PromptRead) -> PromptEvidence,
) -> DecodedLifecycleFields {
    let mut prompt_read = None;
    let (common, signal) = match event_name {
        "PreInvocation" => {
            let Some(invocation) = payloads::parse_invocation(payload) else {
                return DecodedLifecycleFields::default();
            };
            let signal = match invocation.invocation_num {
                Some(0) if invocation.common.runs_compact_command() => {
                    Some(LifecycleSignal::Compacting)
                }
                Some(0) => {
                    prompt_read = Some(
                        invocation
                            .initial_num_steps
                            .map_or(PromptRead::Newest, PromptRead::OpenedBelow),
                    );
                    Some(LifecycleSignal::TurnStarted { turn_id: None })
                }
                _ => None,
            };
            (invocation.common, signal)
        }
        // A user row between the invocation's first step and its answer was taken in while it
        // ran: a steer. Later invocations begin past it, so each steer is reported once.
        "PostInvocation" => {
            let Some(invocation) = payloads::parse_invocation(payload) else {
                return DecodedLifecycleFields::default();
            };
            prompt_read = invocation
                .initial_num_steps
                .filter(|_| !invocation.common.runs_compact_command())
                .map(PromptRead::TakenInFrom);
            let signal = prompt_read.map(|_| LifecycleSignal::TurnStarted { turn_id: None });
            (invocation.common, signal)
        }
        "PostToolUse:edit" | "PostToolUse:mutating" | "PostToolUse:observed" => {
            let Some(tool) = payloads::parse_post_tool(payload) else {
                return DecodedLifecycleFields::default();
            };
            let failed = tool.failed();
            let effect = match event_name {
                "PostToolUse:edit" if !failed => ToolEffect::Editing,
                "PostToolUse:mutating" if !failed => ToolEffect::Mutating,
                _ => ToolEffect::Observed,
            };
            let name = tool.tool_name();
            (
                tool.common,
                Some(LifecycleSignal::ToolUsed {
                    effect,
                    name,
                    native_key: None,
                    turn_id: None,
                }),
            )
        }
        "Stop" => {
            let Some(stop) = payloads::parse_stop(payload) else {
                return DecodedLifecycleFields::default();
            };
            let failed = stop.failed();
            // A bare `/compact` ends on a Stop of its own (`TERMINAL_CUSTOM_HOOK`), which
            // closes the compaction whether or not it carries `fullyIdle`.
            let signal = if stop.common.runs_compact_command() {
                Some(LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed,
                })
            } else {
                stop.fully_idle
                    .map(|fully_idle| LifecycleSignal::TurnEnded {
                        errored: failed,
                        parked_on_background: !fully_idle && !failed,
                        turn_id: None,
                    })
            };
            (stop.common, signal)
        }
        _ => {
            let Some(common) = payloads::parse_common(payload) else {
                return DecodedLifecycleFields::default();
            };
            (common, None)
        }
    };
    observation_with_prompt_reader(common, signal, prompt_read, prompt_reader)
}

fn observation_with_prompt_reader(
    common: payloads::CommonPayload,
    signal: Option<LifecycleSignal>,
    prompt_read: Option<PromptRead>,
    prompt_reader: impl FnOnce(&Path, &str, PromptRead) -> PromptEvidence,
) -> DecodedLifecycleFields {
    let Some(agent_id) = common
        .conversation_id()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
    else {
        return DecodedLifecycleFields::default();
    };
    let worktree_path = common
        .workspace_paths
        .into_iter()
        .find(|path| !path.trim().is_empty());
    let Some(signal) = signal else {
        return DecodedLifecycleFields {
            agent_id: Some(agent_id),
            worktree_path,
            lifecycle: None,
        };
    };
    let transcript_path = common
        .transcript_path
        .filter(|path| !path.trim().is_empty());
    let evidence = match (prompt_read, transcript_path.as_deref()) {
        (Some(read), Some(path)) => prompt_reader(Path::new(path), agent_id.as_str(), read),
        _ => PromptEvidence::Unread,
    };
    let snapshot = matches!(prompt_read, Some(PromptRead::TakenInFrom(_)));
    let prompt = match evidence {
        PromptEvidence::Prompt(prompt) => Some(prompt),
        PromptEvidence::Unread if !snapshot => None,
        // A steer report with no steer, and an opener no prompt opened (a continuation),
        // report nothing.
        PromptEvidence::Unread | PromptEvidence::NoPrompt => {
            return DecodedLifecycleFields {
                agent_id: Some(agent_id),
                worktree_path,
                lifecycle: None,
            };
        }
    };
    let mut observation = AgentLifecycleObservation::new(Some(agent_id.as_str().into()), signal);
    observation.worktree_path = worktree_path.clone();
    observation.prompt = SanitizedPrompt::new(prompt.as_deref());
    observation.transcript_path = transcript_path;
    observation.state_snapshot = snapshot.then_some(true);
    if let Some(model) = common.model_name.as_deref().map(str::trim)
        && !model.is_empty()
    {
        let (base, effort) = split_model_effort(model);
        observation.launch.model = Some(base.to_owned());
        observation.launch.effort = effort.map(ToOwned::to_owned);
    }
    DecodedLifecycleFields {
        agent_id: Some(agent_id),
        worktree_path,
        lifecycle: Some(observation),
    }
}

#[cfg(test)]
fn observe_lifecycle_with_prompt_reader(
    event_name: &str,
    payload: &Value,
    prompt_reader: impl FnOnce(&Path, &str, Option<u64>) -> Option<String>,
) -> Option<AgentLifecycleObservation> {
    decode_lifecycle_fields(event_name, payload, |path, session_id, read| {
        let taken_in_from = match read {
            PromptRead::TakenInFrom(step) => Some(step),
            PromptRead::Newest | PromptRead::OpenedBelow(_) => None,
        };
        prompt_reader(path, session_id, taken_in_from).into()
    })
    .lifecycle
}
