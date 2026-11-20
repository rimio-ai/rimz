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

const HOOK_TIMEOUT_SECS: u64 = 5;
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source antigravity";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source antigravity";
const STATUS_LINE_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity";
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
        config_matcher: Some("^(write_to_file|replace_file_content|multi_replace_file_content)$"),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:edit",
    },
    AntigravityHook {
        hook: HookEventSpec::lifecycle(
            "PostToolUse:mutating",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        )
        .progress(),
        config_event: "PostToolUse",
        config_matcher: Some("^run_command$"),
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
            "^(view_file|list_dir|find_by_name|grep_search|search_web|read_url_content|manage_task|schedule|list_permissions|ask_permission|invoke_subagent|define_subagent|send_message|manage_subagents|ask_question|generate_image|manage_inbox|read_resource|browser_.*)$",
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
    tool_rules: crate::agents::skills::ToolRules::Unsupported,
    host_skills: crate::agents::skills::HostSkills::Unsupported,
    kind: "antigravity",
    aliases: &[],
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
            "run_command",
        ],
        editing: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ],
        blocking: &[],
    },
    capabilities: Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        permission_before_auto_review: false,
        transcript_tail_context: false,
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
            ask: &[],
            auto: &["--mode", "accept-edits"],
            yolo: &["--dangerously-skip-permissions", "--mode", "accept-edits"],
            plan: &["--mode", "plan"],
            ..super::LaunchPermissionArgs::EMPTY
        },
        max_turn_flag: None,
        interrupt_key: None,
        compact_command: None,
        presets: super::PresetMatchers {
            code_mode: super::CodeModeSwitch::Unsupported,
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: Some(super::StaticPresetMatcher::AgentFile(
                agent_file::AGENT_FLAG,
            )),
            ..super::PresetMatchers::EMPTY
        },
        ..super::LaunchSpec::EMPTY
    },
};

const ANTIGRAVITY_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "neutral PreInvocation, PostToolUse, and Stop hooks",
    },
    permission: ConcernCoverage::Partial {
        via: "statusline tool_confirmation_pending routes the card to the native pane",
        gap: "PreToolUse has no behavior-preserving observer decision, so there is no durable ask or permission detail",
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
    compaction: ConcernCoverage::Unsupported {
        reason: "no documented compaction command, event, or transcript marker",
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
    realtime_cost: ConcernCoverage::Partial {
        via: "turn-end transcript token rows priced at list price by exact model",
        gap: "the session total moves only when a turn ends",
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
        limit: "the dollar total updates when a turn ends",
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
        gap: "upstream documents SessionStart, but registration stays unwired until a live capture",
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
    compacting: HookCoverage::Absent {
        reason: "no compaction signal",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "no compaction signal",
    },
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
        super::AdapterConformance {
            classification: ANTIGRAVITY_HOOKS
                .iter()
                .map(|entry| super::hook_types::classification_sample(&entry.hook))
                .collect(),
            spend: Some(super::SpendFixture {
                session_id: "11111111-1111-4111-8111-111111111111",
                file_name: "brain/11111111-1111-4111-8111-111111111111/.system_generated/logs/transcript_full.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-07-13T23:23:09Z","model":"gemini-3-flash-preview","input_tokens":100,"cache_read_tokens":40,"output_tokens":20}"#,
                ),
            }),
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
        let mut decoded = decode_catalog_hook(
            ANTIGRAVITY_HOOKS.iter().map(|entry| &entry.hook),
            event_name,
            None,
        );
        decoded.set_reply(self.payload_hook_reply(event_name, payload));
        let fields = decode_lifecycle_fields(event_name, payload, session::latest_prompt);
        decoded.set_routing(
            HookRouting::session(fields.agent_id.map(Into::into))
                .with_worktree(fields.worktree_path),
        );
        if event_name == "Stop"
            && let Some(observation) = fields.lifecycle.as_ref()
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
            .or_else(|| session::transcript_for_session(parent_id.as_str()))?;
        let correlated = session::correlate_subagent(
            &parent_transcript,
            parent_id.as_str(),
            input.parent_workspace?,
            child_id.as_str(),
            input.child_workspace?,
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
        let Some(parent_transcript) = input
            .parent_transcript_path
            .map(Path::to_path_buf)
            .or_else(|| session::transcript_for_session(input.parent_agent_id.as_str()))
        else {
            return Vec::new();
        };
        session::spawned_subagents(
            &parent_transcript,
            input.parent_agent_id.as_str(),
            parent_workspace,
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

    fn agent_file(
        &self,
        input: &crate::agents::AgentFileInput<'_>,
    ) -> Option<std::result::Result<crate::agents::AgentFile, crate::agents::AgentFileRefusal>>
    {
        Some(agent_file::plan(input))
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        session::resolve_home(None, env.get("HOME").map(std::ffi::OsStr::new))
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
        _login_env: &BTreeMap<String, String>,
    ) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
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
    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        if !payload.is_object() {
            return None;
        }
        let parsed = statusline::StatuslinePayload::deserialize(payload).ok()?;
        let agent_id = parsed.conversation_id.clone()?;
        super::ContextObservation::new(agent_id, parsed.into_context(source, Timestamp::now()))
    }
}

impl crate::agents::capabilities::AccountCapability for AntigravityAdapter {
    fn probe_account(&self, login_env: &BTreeMap<String, String>) -> super::account::AccountProbe {
        let provider_home = self.config_home(login_env);
        usage::read(
            provider_home.clone(),
            login_env.get("HOME").map(String::as_str),
        )
        .account(provider_home.as_deref())
    }

    fn probe_account_usage(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> super::AccountUsageProbe {
        usage::read(
            self.config_home(login_env),
            login_env.get("HOME").map(String::as_str),
        )
        .usage()
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

#[derive(Default)]
struct DecodedLifecycleFields {
    agent_id: Option<String>,
    worktree_path: Option<String>,
    lifecycle: Option<AgentLifecycleObservation>,
}

fn decode_lifecycle_fields(
    event_name: &str,
    payload: &Value,
    prompt_reader: impl FnOnce(&Path, &str) -> Option<String>,
) -> DecodedLifecycleFields {
    let (common, signal) = match event_name {
        "PreInvocation" => {
            let Some(invocation) = payloads::parse_invocation(payload) else {
                return DecodedLifecycleFields::default();
            };
            (
                invocation.common,
                (invocation.invocation_num == Some(0))
                    .then_some(LifecycleSignal::TurnStarted { turn_id: None }),
            )
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
            let fully_idle = stop.fully_idle;
            let failed = stop.failed();
            (
                stop.common,
                fully_idle.map(|fully_idle| LifecycleSignal::TurnEnded {
                    errored: failed,
                    parked_on_background: !fully_idle && !failed,
                    turn_id: None,
                }),
            )
        }
        _ => {
            let Some(common) = payloads::parse_common(payload) else {
                return DecodedLifecycleFields::default();
            };
            (common, None)
        }
    };
    observation_with_prompt_reader(common, signal, prompt_reader)
}

fn observation_with_prompt_reader(
    common: payloads::CommonPayload,
    signal: Option<LifecycleSignal>,
    prompt_reader: impl FnOnce(&Path, &str) -> Option<String>,
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
    let prompt = matches!(signal, LifecycleSignal::TurnStarted { .. })
        .then(|| prompt_reader(Path::new(transcript_path.as_deref()?), agent_id.as_str()))
        .flatten();
    let mut observation = AgentLifecycleObservation::new(Some(agent_id.as_str().into()), signal);
    observation.worktree_path = worktree_path.clone();
    observation.prompt = SanitizedPrompt::new(prompt.as_deref());
    observation.transcript_path = transcript_path;
    observation.launch.model = common.model_name.filter(|model| !model.trim().is_empty());
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
    prompt_reader: impl FnOnce(&Path, &str) -> Option<String>,
) -> Option<AgentLifecycleObservation> {
    decode_lifecycle_fields(event_name, payload, prompt_reader).lifecycle
}
