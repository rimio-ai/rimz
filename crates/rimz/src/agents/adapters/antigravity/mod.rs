//! Antigravity CLI launch, hook, statusline, local-session, transcript, and account adapter.
//! The plugin install, the kept `statusLine` keys, and the `agy -p /usage` envelope were
//! observed on agy 1.5.12; the launch agent file (`agent_file`) on agy 1.5.13.
//!
//! RimZ installs only hooks with documented observer-neutral output. The
//! policy-changing `PreToolUse` decision channel stays untouched; disjoint
//! `PostToolUse` matchers recover tool phase after execution instead. The
//! one additive reply, a supervised child's deadline warning or the
//! smart-compaction reminder, is injected on `PreInvocation` as a system
//! message step.

mod agent_file;
mod folder_trust;
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
    HookContextReply, HookCoverage, LifecycleAnnotations, LocalSessionDiscovery, PlanLabel,
    RemoteControlCapability, RequestStartRecord, SamePaneSessionPolicy, ThreadKey,
    ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, PROVIDER_REST_EVENT, decode_catalog_hook};
use super::lifecycle::{LifecycleSignal, ToolEffect};
use super::{
    AgentLifecycleObservation, AgentTurnError, BackgroundShellReport, HookOutput, HookRouting,
    LocalSessionObservation, Result, SanitizedPrompt, SpawnedSubagent, SubagentCorrelation,
    SubagentCorrelationInput, SubagentIdentity, SubagentSpawnInput, TranscriptMessage,
    TurnErrorClass, non_empty_trimmed, resolve_subagent_identity, sanitize_user_prompt,
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
const MOUNTED_LOGIN_FILES: [MountedLoginFile; 2] = [
    MountedLoginFile {
        name: "antigravity-oauth-token",
        required: true,
    },
    MountedLoginFile {
        name: "installation_id",
        required: false,
    },
];
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
        // The `PreInvocation` reply injects a deadline warning or the
        // smart-compaction reminder as a system message the next model call
        // reads; `PostToolUse` replies carry no context.
        hook_context: Some(HookContextReply::InjectedSystemMessage {
            event: "PreInvocation",
        }),
        prompt_context: false,
        native_ask_ui: true,
        permission_before_auto_review: false,
        transcript_tail_context: true,
        context_lags_compaction: true,
        lifecycle_reports_fill: false,
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
        request_start_record: RequestStartRecord::Unrecorded,
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
            home_series: Some("Gemini"),
            line_names: &["Neon", "Hydrogen", "Argon", "Carbon"],
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
        compact_command: Some(super::CompactCommand {
            command: COMPACT_COMMAND,
            instruction: super::CompactInstruction::NextPrompt,
            steer_checked: true,
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
        message_keys: crate::agents::MessageKeys {
            send_now: &[
                crate::pane::keys::KeyStep::now(crate::pane::keys::NamedKey::Enter),
                crate::pane::keys::KeyStep::after_ms(1000, crate::pane::keys::NamedKey::AltM),
            ],
            queue: &[crate::pane::keys::KeyStep::now(
                crate::pane::keys::NamedKey::Enter,
            )],
            implemented: true,
            ..crate::agents::MessageKeys::NOT_YET_IMPLEMENTED
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
        via: "PreInvocation opener for a bare /compact and Stop with TERMINAL_CUSTOM_HOOK",
    },
    subagents: ConcernCoverage::Partial {
        via: "child hooks and parent reverse pass joined to invoke_subagent transcript results and 1.5.x GENERIC acknowledgements",
        gap: "the parent-transcript request and result shapes are live-verified rather than a documented stable wire contract",
    },
    launch_reminders: ConcernCoverage::Wired {
        via: "a fresh launch's agent file in RimZ's agy plugin, selected by --agent",
    },
    background_parking: ConcernCoverage::Wired {
        via: "Stop.fullyIdle parks a clean foreground stop while background work remains",
    },
    background_shells: ConcernCoverage::Wired {
        via: "Stop transcript snapshots (emptied on fullyIdle) and PostToolUse run_command and manage_task kill",
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
        limit: "a child's name and task wait for its parent's next hook or stop, and a child that opens beside a second conversation on its pane stays unlinked until then",
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
        via: "child PreInvocation and parent reverse pass joined to invoke_subagent transcript results or 1.5.x GENERIC acknowledgements",
        gap: "the parent-transcript request and result shapes are live-verified rather than documented",
    },
    subagent_stopped: HookCoverage::Derived {
        via: "child unparked Stop, 1.1.2 INVOKE_SUBAGENT results, or unparked parent turn end while named in the parent transcript",
        gap: "the parent-transcript request and result shapes are live-verified rather than documented",
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
        let mut fields = decode_lifecycle_fields(event_name, payload, |path, id, read| {
            home.as_deref().map_or(PromptEvidence::Unread, |home| {
                session::read_prompt_under(home, path, id, read)
            })
        });
        decoded.set_routing(
            HookRouting::session(fields.agent_id.map(Into::into))
                .with_worktree(fields.worktree_path),
        );
        if event_name == PROVIDER_REST_EVENT
            && let Ok(statusline) = statusline::StatuslinePayload::deserialize(payload)
            && let Some(rest) = statusline.provider_rest()
        {
            decoded.set_provider_rest(rest);
        }
        if event_name == "PreInvocation"
            && payloads::parse_invocation(payload)
                .is_some_and(|invocation| invocation.opens_native_subagent())
        {
            decoded.set_native_subagent_opener();
        }
        if let Some(observation) = fields.lifecycle.as_mut()
            && matches!(observation.signal, LifecycleSignal::TurnEnded { .. })
        {
            decoded.set_final_message(
                observation
                    .transcript_path
                    .as_deref()
                    .and_then(|path| session::last_assistant_message(Path::new(path))),
            );
            if observation.background_shells.is_none() {
                observation.background_shells = home
                    .as_deref()
                    .zip(observation.transcript_path.as_deref())
                    .zip(observation.agent_id.as_ref())
                    .and_then(|((home, path), id)| {
                        session::running_background_shells_under(
                            home,
                            Path::new(path),
                            id.as_str(),
                            Timestamp::now(),
                        )
                    })
                    .map(|shells| BackgroundShellReport::Snapshot { shells });
            }
        }
        decoded.set_turn_error(fields.turn_error);
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

    fn folder_trust(
        &self,
        cwd: &Path,
        repo_root: Option<&Path>,
        login_env: &BTreeMap<String, String>,
    ) -> Option<crate::agents::FolderTrust> {
        Some(folder_trust::folder_trust(cwd, repo_root, login_env))
    }
}

impl crate::agents::capabilities::LaunchCapability for AntigravityAdapter {
    fn project_skills(&self) -> crate::agents::ProjectSkills {
        crate::agents::ProjectSkills::Native
    }

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

    fn shared_login_home_env_key(&self) -> Option<&'static str> {
        Some(session::LOGIN_HOME_ENV)
    }

    fn mounted_login_files(&self) -> &'static [MountedLoginFile] {
        &MOUNTED_LOGIN_FILES
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        session::login_home(env)
    }

    fn config_home_env_keys(&self) -> &'static [&'static str] {
        &[session::HOME_ENV]
    }

    /// agy reads no variable or flag that moves its data directory (1.5.16), so a named
    /// account mounts over it with `HOME` left as the user's: login files for a shared
    /// account, or the whole account directory for a standalone one.
    fn mounted_home_target(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        session::view_home(env)
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
        permissions::pin_modes(&ASK_SETTINGS, paths, extra_args, artifact)
    }

    fn bridge_saved_settings_args(
        &self,
        login_env: &BTreeMap<String, String>,
        paths: (&Path, &Path),
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        folder_trust::bridge(login_env, paths, extra_args, artifact)
    }

    fn allow_team_notes_args(
        &self,
        run: &Path,
        paths: (&Path, &Path),
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        permissions::allow_write_dirs([run], paths, extra_args, artifact)
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

    fn configure_sandbox_settings(
        &self,
        paths: (&Path, &Path),
        roots: &[PathBuf; 3],
        _isolation: crate::config::Isolation,
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        permissions::allow_write_dirs(
            roots.iter().map(PathBuf::as_path),
            paths,
            extra_args,
            artifact,
        )?;
        if let Some(common_dir) = crate::worktree::common_git_dir(paths.0) {
            permissions::allow_read_dirs([common_dir.as_path()], paths, extra_args, artifact)?;
        }
        Ok(())
    }

    fn allow_routine_rimz_args(
        &self,
        paths: (&Path, &Path),
        _skill_roots: (Option<&Path>, Option<&crate::agents::skills::SkillChain>),
        _dirs: &[PathBuf; 2],
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<(), crate::agents::skills::LaunchSettingsErr> {
        permissions::allow_routine_rimz(paths, extra_args, artifact)
    }

    /// The list rides the agent file (`agent_file`) and the server the rimz plugin's
    /// `mcp_config.json` (`install`), while `mcp_server_args` merges the per-launch allow rule
    /// into `--settings` when the launch serves RimZ's tools with `allow-routine-rimz` on.
    fn takes_mcp_servers(&self) -> bool {
        true
    }

    fn mcp_server_args(
        &self,
        launch: &crate::agents::capabilities::McpLaunch<'_>,
        paths: (&Path, &Path),
        extra_args: &mut Vec<String>,
        artifact: &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ) -> std::result::Result<
        (Vec<String>, Vec<crate::agents::capabilities::UnappliedMark>),
        crate::agents::skills::LaunchSettingsErr,
    > {
        if launch.server.as_ref().is_some_and(|server| server.grant) {
            permissions::allow_mcp_server(paths, extra_args, artifact)?;
        }
        Ok((Vec::new(), Vec::new()))
    }

    fn serves_mcp_from_launch_env(&self) -> bool {
        true
    }

    fn mcp_server_name(&self) -> &'static str {
        MCP_SERVER_NAME
    }

    /// The global root agy's own built-in skills name. agy also reads
    /// `~/.gemini/antigravity-cli/skills` (above this root on a duplicate name), `~/.gemini/skills`
    /// (below it, agy 1.5.14), and a plugin's `skills/`, which the scoped view leaves native.
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

    fn supports_resume(&self) -> bool {
        true
    }

    fn resumes_session(&self, session_id: &str) -> bool {
        session::valid_conversation_id(session_id)
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
        let credential_home = session::credential_home(login_env);
        usage::read(credential_home.clone(), login_env).account(credential_home.as_deref())
    }

    fn foreign_login(&self, login_env: &BTreeMap<String, String>) -> Option<String> {
        let mount = session::account_mount(login_env)?;
        if usage::holds_credential(mount.account()) {
            return None;
        }
        usage::read(session::credential_home(login_env), login_env)
            .launch_refusal()
            .map(str::to_owned)
    }

    fn probe_account_usage(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> super::AccountUsageProbe {
        usage::read(session::credential_home(login_env), login_env).usage()
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

/// The `permissions.allow` rule for every tool of the plugin's server.
fn mcp_allow_rule() -> String {
    format!("mcp({MCP_SERVER_NAME}/*)")
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
    turn_error: Option<AgentTurnError>,
}

/// The marker for a string `Stop.error`. A stop naming a spent quota is a rate limit,
/// whatever code comes first, labelled with the provider's sentence so the card shows the quota
/// rather than the transport prefix. Any other stop is classified by agy's status form
/// ([`status_verdict`]) over the whole message. The marker carries the refill a quota stop
/// names, so the quota stop confirms no model refusal.
fn antigravity_turn_error(
    message: &str,
    quota: Option<&payloads::QuotaStop>,
) -> Option<AgentTurnError> {
    let verdict = quota
        .map(|_| TurnErrorClass::PausedRateLimit)
        .or_else(|| status_verdict(message));
    let message = crate::agents::context::cap_turn_error_label(message)?;
    let label = quota
        .and_then(|quota| quota.label.as_deref())
        .and_then(crate::agents::context::cap_turn_error_label)
        .unwrap_or(message);
    let at = Timestamp::now();
    let refill_at = quota
        .and_then(|quota| quota.refill_in_secs)
        .and_then(|secs| i64::try_from(secs).ok())
        .and_then(|secs| at.checked_add(jiff::SignedDuration::from_secs(secs)).ok());
    Some(AgentTurnError {
        refill_at,
        ..AgentTurnError::classified(at, Some(label), verdict)
    })
}

/// The gRPC status names agy prints for a transient server error, in the camel case of its
/// `transport error (<status>)` templates and the upper snake case of `<STATUS> (code <code>)`.
const OVERLOAD_STATUSES: [&str; 6] = [
    "Unavailable",
    "Internal",
    "DeadlineExceeded",
    "UNAVAILABLE",
    "INTERNAL",
    "DEADLINE_EXCEEDED",
];

/// agy's verdict from the status form its stop templates print. A code
/// (`transport error (<status>, HTTP <code>)`, `transport error (HTTP <code>)`, or
/// `<STATUS> (code <code>)`, the last only right after an upper-snake status) decides alone: 429
/// is a rate limit, a 5xx an overload, and any other code a failure. A code that is not three
/// digits, or a `(code <code>)` after anything but such a status, is no code. Without a code, an
/// `OVERLOAD_STATUSES` name as the whole `transport error (<status>)` is an overload. Any other
/// text gives no verdict.
fn status_verdict(error: &str) -> Option<TurnErrorClass> {
    const TRANSPORT: &str = "transport error (";
    const GRPC_CODE: &str = " (code ";
    let enclosed = |marker: &'static str| {
        error.match_indices(marker).filter_map(move |(at, _)| {
            let (inside, _) = error[at + marker.len()..].split_once(')')?;
            Some((&error[..at], inside))
        })
    };
    let code = |digits: &str| {
        (digits.len() == 3 && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| digits.parse::<u16>().ok())
            .flatten()
    };
    let after_status = |before: &str| {
        let rest = before.trim_end_matches(|ch: char| ch.is_ascii_uppercase() || ch == '_');
        before[rest.len()..]
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
            && !rest.ends_with(|ch: char| ch.is_ascii_alphanumeric())
    };
    let http_codes = enclosed(TRANSPORT).filter_map(|(_, status)| {
        status
            .rsplit_once(", HTTP ")
            .map(|(_, digits)| digits)
            .or_else(|| status.strip_prefix("HTTP "))
            .and_then(code)
    });
    let grpc_codes = enclosed(GRPC_CODE)
        .filter(|(before, _)| after_status(before))
        .filter_map(|(_, digits)| code(digits));
    if let Some(code) = http_codes.chain(grpc_codes).next() {
        return Some(match code {
            429 => TurnErrorClass::PausedRateLimit,
            500..=599 => TurnErrorClass::PausedOverloaded,
            _ => TurnErrorClass::Failed,
        });
    }
    enclosed(TRANSPORT)
        .any(|(_, status)| OVERLOAD_STATUSES.contains(&status))
        .then_some(TurnErrorClass::PausedOverloaded)
}

fn decode_lifecycle_fields(
    event_name: &str,
    payload: &Value,
    prompt_reader: impl FnOnce(&Path, &str, PromptRead) -> PromptEvidence,
) -> DecodedLifecycleFields {
    let mut prompt_read = None;
    let mut background_shells = None;
    let mut turn_error = None;
    let (common, signal) = match event_name {
        "PreInvocation" => {
            let Some(invocation) = payloads::parse_invocation(payload) else {
                return DecodedLifecycleFields::default();
            };
            let signal = match invocation.invocation_num {
                Some(0) => {
                    if let Some(bound) = invocation.initial_num_steps {
                        prompt_read = Some(PromptRead::OpenedBelow(bound));
                        Some(LifecycleSignal::TurnStarted { turn_id: None })
                    } else if invocation.common.runs_compact_command() {
                        Some(LifecycleSignal::Compacting)
                    } else {
                        prompt_read = Some(PromptRead::Newest);
                        Some(LifecycleSignal::TurnStarted { turn_id: None })
                    }
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
            prompt_read = invocation.initial_num_steps.map(PromptRead::TakenInFrom);
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
            background_shells = tool
                .common
                .conversation_id()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .and_then(|id| tool.finished_task_id(id))
                .map(|id| BackgroundShellReport::Finished { ids: vec![id] });
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
            // A bare `/compact` closes only on `TERMINAL_CUSTOM_HOOK`, whether or not it carries
            // `fullyIdle`. Every other `Stop` follows the turn rule, even while `lastUserInput`
            // still names `/compact` on a post-compaction continuation.
            let signal = if stop.closes_compaction() {
                Some(LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed,
                    echo: false,
                })
            } else {
                if stop.fully_idle == Some(true) {
                    background_shells =
                        Some(BackgroundShellReport::Snapshot { shells: Vec::new() });
                }
                let quota = stop.quota_stop();
                let signal = stop
                    .fully_idle
                    .map(|fully_idle| LifecycleSignal::TurnEnded {
                        errored: failed,
                        parked_on_background: !fully_idle && !failed,
                        turn_id: None,
                    });
                if matches!(
                    signal,
                    Some(LifecycleSignal::TurnEnded { errored: true, .. })
                ) {
                    turn_error = stop
                        .error_message()
                        .and_then(|message| antigravity_turn_error(message, quota.as_ref()));
                }
                signal
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
    observation_with_prompt_reader(
        common,
        signal,
        prompt_read,
        background_shells,
        turn_error,
        prompt_reader,
    )
}

fn observation_with_prompt_reader(
    common: payloads::CommonPayload,
    signal: Option<LifecycleSignal>,
    prompt_read: Option<PromptRead>,
    background_shells: Option<BackgroundShellReport>,
    turn_error: Option<AgentTurnError>,
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
    let runs_compact = common.runs_compact_command();
    let worktree_path = common
        .workspace_paths
        .into_iter()
        .find(|path| !path.trim().is_empty());
    let Some(signal) = signal else {
        return DecodedLifecycleFields {
            agent_id: Some(agent_id),
            worktree_path,
            lifecycle: None,
            turn_error: None,
        };
    };
    let transcript_path = common
        .transcript_path
        .filter(|path| !path.trim().is_empty());
    let evidence = match (prompt_read, transcript_path.as_deref()) {
        (Some(read), Some(path)) => prompt_reader(Path::new(path), agent_id.as_str(), read),
        _ => PromptEvidence::Unread,
    };
    let mut snapshot = matches!(prompt_read, Some(PromptRead::TakenInFrom(_)));
    let (signal, prompt) = match evidence {
        PromptEvidence::Prompt(prompt) => (signal, Some(prompt)),
        PromptEvidence::Compact => (LifecycleSignal::Compacting, None),
        // A steered `/compact` (or `/compact <message>`) compacts inside an open turn without
        // firing a compact `PreInvocation` or `Stop`: its `PostInvocation` carries the
        // `CHECKPOINT` close as a full lifecycle edge (not a state snapshot) plus any
        // continuation prompt carried on the same `/compact <message>` line.
        PromptEvidence::CompactionEnded { prompt } => {
            snapshot = false;
            (
                LifecycleSignal::CompactionEnded {
                    auto: Some(true),
                    failed: false,
                    echo: false,
                },
                prompt,
            )
        }
        PromptEvidence::Unread if !snapshot => {
            let signal = if matches!(prompt_read, Some(PromptRead::OpenedBelow(_))) && runs_compact
            {
                LifecycleSignal::Compacting
            } else {
                signal
            };
            (signal, None)
        }
        // A steer report with no steer, and an opener no prompt opened (a continuation),
        // report nothing.
        PromptEvidence::Unread | PromptEvidence::NoPrompt => {
            return DecodedLifecycleFields {
                agent_id: Some(agent_id),
                worktree_path,
                lifecycle: None,
                turn_error: None,
            };
        }
    };
    let mut observation = AgentLifecycleObservation::new(Some(agent_id.as_str().into()), signal);
    observation.worktree_path = worktree_path.clone();
    observation.prompt = SanitizedPrompt::new(prompt.as_deref());
    observation.transcript_path = transcript_path;
    observation.state_snapshot = snapshot.then_some(true);
    observation.background_shells = background_shells;
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
        turn_error,
    }
}

/// The catalog event whose `PostToolUse` matcher names `tool`. Each matcher is an anchored
/// alternation of names, and a name ending in `.*` matches by prefix.
#[cfg(test)]
fn post_tool_event(tool: &str) -> Option<&'static str> {
    ANTIGRAVITY_HOOKS
        .iter()
        .filter(|hook| hook.config_event == "PostToolUse")
        .find(|hook| {
            let names = hook.config_matcher.unwrap();
            let names = names.strip_prefix('^').unwrap().strip_suffix('$').unwrap();
            let names = names
                .strip_prefix('(')
                .and_then(|names| names.strip_suffix(')'))
                .unwrap_or(names);
            names.split('|').any(|name| match name.strip_suffix(".*") {
                Some(prefix) => tool.starts_with(prefix),
                None => tool == name,
            })
        })
        .map(|hook| hook.hook.event)
}

#[cfg(test)]
pub(in crate::agents) fn catalog_event_for_capture(
    native_event: &str,
    payload: &Value,
) -> &'static str {
    if native_event == "PostToolUse" {
        let tool = payload
            .get("toolCall")
            .and_then(|call| call.get("name"))
            .and_then(Value::as_str);
        return tool.and_then(post_tool_event).unwrap_or_else(|| {
            panic!("captured PostToolUse (tool {tool:?}) matches no installed Antigravity hook")
        });
    }
    ANTIGRAVITY_HOOKS
        .iter()
        .find(|entry| entry.config_event == native_event && entry.config_matcher.is_none())
        .map(|entry| entry.hook.event)
        .unwrap_or_else(|| {
            panic!("captured event {native_event:?} matches no installed Antigravity hook")
        })
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
