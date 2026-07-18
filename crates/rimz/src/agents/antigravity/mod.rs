//! Antigravity CLI 1.1.2 launch, hook, statusline, local-session, and transcript adapter.
//!
//! RimZ installs only hooks with documented observer-neutral output. The
//! policy-changing `PreToolUse` decision channel stays untouched; disjoint
//! `PostToolUse` matchers recover tool phase after execution instead.

mod install;
mod local_api;
mod payloads;
mod session;
mod statusline;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

#[cfg(test)]
use super::AgentHookClass;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    SamePaneSessionPolicy, ThreadKey, ToolClassification,
};
use super::hook_types::{HookRecord, classify_catalog_entry, hook_record};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentAdapter, AgentContext, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, LocalSessionObservation, Result, SpawnedSubagent,
    SubagentCorrelation, SubagentCorrelationInput, SubagentIdentity, SubagentSpawnInput,
    TranscriptMessage, non_empty_trimmed, resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;

pub const SUPPORTED_VERSION: &str = "1.1.2";
const HOOK_TIMEOUT_SECS: u64 = 5;
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source antigravity";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source antigravity";
const STATUS_LINE_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity";

pub(super) struct AntigravityHook {
    pub(super) hook: HookRecord,
    pub(super) config_event: &'static str,
    pub(super) config_matcher: Option<&'static str>,
    pub(super) command: &'static str,
}

pub(super) const ANTIGRAVITY_HOOKS: [AntigravityHook; 6] = [
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "PreInvocation",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","invocationNum":0}"#
        ),
        config_event: "PreInvocation",
        config_matcher: None,
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PreInvocation",
    },
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "PostToolUse:edit",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        ),
        config_event: "PostToolUse",
        config_matcher: Some("^(write_to_file|replace_file_content|multi_replace_file_content)$"),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:edit",
    },
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "PostToolUse:mutating",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        ),
        config_event: "PostToolUse",
        config_matcher: Some("^run_command$"),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:mutating",
    },
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "PostToolUse:observed",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        ),
        config_event: "PostToolUse",
        config_matcher: Some(
            "^(view_file|list_dir|find_by_name|grep_search|search_web|read_url_content|manage_task|schedule|list_permissions|ask_permission|invoke_subagent|define_subagent|send_message|manage_subagents|ask_question|generate_image)$",
        ),
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:observed",
    },
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "PostInvocation",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl"}"#
        ),
        config_event: "PostInvocation",
        config_matcher: None,
        command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostInvocation",
    },
    AntigravityHook {
        hook: hook_record!(
            lifecycle,
            "Stop",
            r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","fullyIdle":true,"terminationReason":"model_stop"}"#
        ),
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

static ANTIGRAVITY_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "antigravity",
    display_name: "Antigravity",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x42, 0x85, 0xf4),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
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
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: SamePaneSessionPolicy::FollowLatest,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: ANTIGRAVITY_COVERAGE,
    lifecycle_hooks: ANTIGRAVITY_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["agy"],
    bin_names: &["agy"],
    extra_bin_dirs: &[".local/bin"],
    activity_events: &ANTIGRAVITY_EVENT_NAMES,
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("agy"),
        fixed_args: &[],
        prompt: super::PromptStyle::Flag("--prompt-interactive"),
        resume: None,
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--mode", "accept-edits"],
            yolo: &["--dangerously-skip-permissions"],
            plan: &["--mode", "plan"],
        },
        ping_args: None,
        max_turn_flag: None,
        compact_command: None,
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const ANTIGRAVITY_COVERAGE: IntegrationCoverage = IntegrationCoverage {
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
    background_parking: ConcernCoverage::Wired {
        via: "Stop.fullyIdle parks a clean foreground stop while background work remains",
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
        via: "current statusline usage is priced through the local model price book",
        gap: "no cumulative session or provider billing ledger is published",
    },
    rich_context: ConcernCoverage::Wired {
        via: "custom statusline context plus local account identity and subscription quota",
    },
    hook_install: ConcernCoverage::Wired {
        via: "idempotent global hooks.json merge plus reversible statusLine wrap",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "quota is work-metered and no cumulative billing ledger is published",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no CLI remote-control host is documented",
    },
};

const ANTIGRAVITY_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Derived {
        via: "first PreInvocation create-on-miss + validated local conversation discovery",
        gap: "Antigravity publishes no session-only registration event",
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
pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &ANTIGRAVITY_DESCRIPTOR
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        local_api::probe_account()
            .map(super::account::AccountProbe::Found)
            .unwrap_or(super::account::AccountProbe::Unavailable)
    }

    fn probe_account_usage(&self) -> super::AccountUsageProbe {
        local_api::probe_account_usage()
    }

    fn probe_version(&self) -> Option<String> {
        None
    }

    #[cfg(test)]
    fn local_session_fixture(&self) -> Option<LocalSessionObservation> {
        Some(session::fixture_observation())
    }

    #[cfg(test)]
    fn context_cost_fixture(&self) -> Option<super::ContextCostFixture> {
        Some(super::ContextCostFixture {
            payload: serde_json::json!({
                "model": {"id": "gemini-3-flash-preview"},
                "context_window": {
                    "current_usage": {
                        "input_tokens": 100,
                        "output_tokens": 20,
                        "cache_creation_input_tokens": 30,
                        "cache_read_input_tokens": 40
                    }
                }
            }),
        })
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_catalog_entry(
            ANTIGRAVITY_HOOKS
                .iter()
                .find(|entry| entry.hook.event == event_name)
                .map(|entry| &entry.hook),
            event_name,
            None,
        )
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        ANTIGRAVITY_EVENT_NAMES.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        ANTIGRAVITY_HOOKS
            .iter()
            .map(|entry| super::hook_types::classification_sample(&entry.hook))
            .collect()
    }

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(match event_name {
            "Stop" => Some(serde_json::json!({ "decision": "" })),
            event if ANTIGRAVITY_EVENT_NAMES.contains(&event) => Some(serde_json::json!({})),
            _ => None,
        })
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        observe_lifecycle_with_prompt_reader(event_name, payload, session::latest_prompt)
    }

    fn correlate_subagent(
        &self,
        input: SubagentCorrelationInput<'_>,
    ) -> Option<SubagentCorrelation> {
        let (child_id, parent_id) = match resolve_subagent_identity(
            self.descriptor().kind,
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

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        if !payload.is_object() {
            return None;
        }
        serde_json::from_value::<statusline::StatuslinePayload>(payload.clone())
            .ok()
            .map(|payload| payload.into_context(source, Timestamp::now()))
    }

    fn context_cost(&self, payload: &Value, prices: &super::PriceBook) -> Option<super::AgentCost> {
        if !payload.is_object() {
            return None;
        }
        serde_json::from_value::<statusline::StatuslinePayload>(payload.clone())
            .ok()?
            .cost(prices)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        _payload: &Value,
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        if event_name != "Stop" {
            return None;
        }
        session::last_assistant_message(Path::new(observation.transcript_path.as_deref()?))
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| session::valid_transcript(path, session_id)) {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session(session_id)
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
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

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install(&install::hooks_path()?, &install::settings_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview(&install::hooks_path()?, &install::settings_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall(&install::hooks_path()?, &install::settings_path()?)
    }

    fn hooks_installed(&self) -> bool {
        install::hooks_path()
            .and_then(|hooks| {
                install::settings_path().map(|settings| install::installed(&hooks, &settings))
            })
            .unwrap_or(false)
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::hooks_path()
            .and_then(|hooks| {
                install::settings_path().map(|settings| install::managed(&hooks, &settings))
            })
            .unwrap_or(false)
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        install::settings_path()
            .ok()
            .and_then(|path| install::wrapped_statusline_command(&path))
    }
}

fn observe_lifecycle_with_prompt_reader(
    event_name: &str,
    payload: &Value,
    prompt_reader: impl FnOnce(&Path, &str) -> Option<String>,
) -> Option<AgentLifecycleObservation> {
    let (common, signal) = match event_name {
        "PreInvocation" => {
            let invocation = payloads::parse_invocation(payload)?;
            (invocation.invocation_num == Some(0))
                .then_some((invocation.common, LifecycleSignal::TurnStarted))?
        }
        "PostToolUse:edit" | "PostToolUse:mutating" | "PostToolUse:observed" => {
            let tool = payloads::parse_post_tool(payload)?;
            let failed = tool.failed();
            let (mutates, edits) = match event_name {
                "PostToolUse:edit" if !failed => (true, true),
                "PostToolUse:mutating" if !failed => (true, false),
                _ => (false, false),
            };
            (
                tool.common,
                LifecycleSignal::ToolUsed {
                    mutates,
                    edits,
                    native_key: None,
                },
            )
        }
        "Stop" => {
            let stop = payloads::parse_stop(payload)?;
            let fully_idle = stop.fully_idle?;
            let failed = stop.failed();
            (
                stop.common,
                LifecycleSignal::TurnEnded {
                    errored: failed,
                    parked_on_background: !fully_idle && !failed,
                },
            )
        }
        _ => return None,
    };
    observation_with_prompt_reader(common, signal, prompt_reader)
}

fn observation_with_prompt_reader(
    common: payloads::CommonPayload,
    signal: LifecycleSignal,
    prompt_reader: impl FnOnce(&Path, &str) -> Option<String>,
) -> Option<AgentLifecycleObservation> {
    let agent_id = common
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?
        .to_owned();
    let transcript_path = common
        .transcript_path
        .filter(|path| !path.trim().is_empty());
    let prompt = matches!(signal, LifecycleSignal::TurnStarted)
        .then(|| prompt_reader(Path::new(transcript_path.as_deref()?), agent_id.as_str()))
        .flatten();
    let mut observation = AgentLifecycleObservation::new(Some(agent_id.as_str().into()), signal);
    observation.worktree_path = common
        .workspace_paths
        .into_iter()
        .find(|path| !path.trim().is_empty());
    observation.prompt = prompt;
    observation.transcript_path = transcript_path;
    observation.launch.model = common.model_name.filter(|model| !model.trim().is_empty());
    Some(observation)
}
