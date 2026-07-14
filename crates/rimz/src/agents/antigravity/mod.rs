//! Antigravity CLI 1.1.2 launch, hook, statusline, local-session, and transcript adapter.
//!
//! Rimz installs only hooks with documented observer-neutral output. The
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

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentAdapter, AgentContext, AgentHookClass, AgentLifecycleObservation, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, LocalSessionObservation,
    PresetArgMatcher, PresetField, Result, TranscriptMessage,
};
use crate::harness::run::PermissionMode;

pub const SUPPORTED_VERSION: &str = "1.1.2";
const HOOK_TIMEOUT_SECS: u64 = 5;
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source antigravity";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source antigravity";
const STATUS_LINE_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity";
const PRE_INVOCATION_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PreInvocation";
const POST_TOOL_EDIT_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:edit";
const POST_TOOL_MUTATING_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:mutating";
const POST_TOOL_OBSERVED_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostToolUse:observed";
const POST_INVOCATION_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PostInvocation";
const STOP_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event Stop";
const POST_TOOL_EDIT_MATCHER: &str =
    "^(write_to_file|replace_file_content|multi_replace_file_content)$";
const POST_TOOL_MUTATING_MATCHER: &str = "^run_command$";
const POST_TOOL_OBSERVED_MATCHER: &str = "^(view_file|list_dir|find_by_name|grep_search|search_web|read_url_content|manage_task|schedule|list_permissions|ask_permission|invoke_subagent|define_subagent|send_message|manage_subagents|ask_question|generate_image)$";
const INSTALLED_EVENT_LABELS: &[&str] = &[
    "PreInvocation",
    "PostToolUse:edit",
    "PostToolUse:mutating",
    "PostToolUse:observed",
    "PostInvocation",
    "Stop",
];
const LIFECYCLE_EVENTS: &[&str] = INSTALLED_EVENT_LABELS;

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
    activity_events: INSTALLED_EVENT_LABELS,
    thread_key: ThreadKey::PerFile,
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
    subagents: ConcernCoverage::Unsupported {
        reason: "verified local records expose no stable child identity and parent relation",
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
    subagent_started: HookCoverage::Absent {
        reason: "no verified child identity relation",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "no verified child identity relation",
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

    fn probe_realtime_account_usage(
        &self,
        _runtime: &crate::RuntimePaths,
    ) -> Option<super::AccountUsageSnapshot> {
        local_api::probe_rate_limits()
            .ok()
            .map(|rate_limits| super::AccountUsageSnapshot {
                rate_limits: Some(rate_limits),
                ..Default::default()
            })
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
        ClassifiedHook {
            class: if LIFECYCLE_EVENTS.contains(&event_name) {
                AgentHookClass::Lifecycle
            } else {
                AgentHookClass::Unknown
            },
            ask_kind: None,
            event_name: event_name.to_owned(),
        }
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        INSTALLED_EVENT_LABELS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::ClassificationSample;
        let common = serde_json::json!({
            "conversationId": "11111111-1111-4111-8111-111111111111",
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": "/tmp/transcript_full.jsonl",
        });
        INSTALLED_EVENT_LABELS
            .iter()
            .map(|event| {
                let mut payload = common.clone();
                if event.starts_with("PreInvocation") {
                    payload["invocationNum"] = Value::from(0);
                }
                if *event == "Stop" {
                    payload["fullyIdle"] = Value::Bool(true);
                    payload["terminationReason"] = Value::String("model_stop".to_owned());
                }
                ClassificationSample::new(event, payload, AgentHookClass::Lifecycle, None)
            })
            .collect()
    }

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(match event_name {
            "Stop" => Some(serde_json::json!({ "decision": "" })),
            event if LIFECYCLE_EVENTS.contains(&event) => Some(serde_json::json!({})),
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
        let transcript = std::fs::read_to_string(observation.transcript_path.as_deref()?).ok()?;
        self.parse_transcript_messages(&transcript)
            .into_iter()
            .rev()
            .find(|message| message.role == super::TranscriptRole::Assistant)
            .map(|message| message.text)
    }

    fn discover_local_sessions(&self, workspace: &Path) -> Vec<LocalSessionObservation> {
        session::discover(workspace)
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

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Auto => vec!["--mode".to_owned(), "accept-edits".to_owned()],
            PermissionMode::Plan => vec!["--mode".to_owned(), "plan".to_owned()],
            PermissionMode::Yolo => vec!["--dangerously-skip-permissions".to_owned()],
        }
    }

    fn preset_arg_matcher(&self, field: PresetField) -> Option<PresetArgMatcher> {
        (field == PresetField::Model).then(|| PresetArgMatcher::Flag(vec!["--model".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["agy".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.extend(["--prompt-interactive".to_owned(), prompt.to_owned()]);
        }
        Some(argv)
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
            (tool.common, LifecycleSignal::ToolUsed { mutates, edits })
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
