//! Cursor CLI hook adapter.
//!
//! Cursor's native hooks expose session, turn, tool, child, exit, and
//! compaction-open signals. A version-pinned local pending-call reader derives
//! pane-only `AskQuestion` waits; permission, structured answers,
//! machine-readable spend, and post-compaction events remain explicit gaps.

mod account;
mod install;
mod payloads;
mod session;
mod statusline;
mod transcript;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
#[cfg(test)]
use serde_json::json as test_json;
use serde_json::{Value, json};

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentAdapter, AgentContext, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, LocalSessionObservation, LocallyPricedTurnCost,
    PriceBook, Result, SubagentIdentity, classify_agent_hook, locate_binary, non_empty_trimmed,
    resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;

static CURSOR_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "cursor",
    display_name: "Cursor",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xe8, 0xe8, 0xe8),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Cursor" },
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["Shell", "Write", "Delete"],
        editing: &["Write", "Delete"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: false,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: CURSOR_COVERAGE,
    lifecycle_hooks: CURSOR_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["cursor-agent", "agent"],
    bin_names: &["cursor-agent", "agent"],
    extra_bin_dirs: &[],
    activity_events: &[
        "sessionStart",
        "beforeSubmitPrompt",
        "postToolUse",
        "postToolUseFailure",
        "afterAgentResponse",
        "stop",
        "subagentStart",
        "subagentStop",
    ],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("agent"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: None,
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--auto-review"],
            yolo: &["--force", "--sandbox", "disabled"],
            plan: &["--mode=plan"],
        },
        ping_args: None,
        max_turn_flag: None,
        compact_command: Some("/summarize"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const CURSOR_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "sessionStart/beforeSubmitPrompt/stop including native interruption",
    },
    permission: ConcernCoverage::Unsupported {
        reason: "no local permission hook; ACP-only",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no local plan-approval hook; ACP-only",
    },
    user_question: ConcernCoverage::Partial {
        via: "version-pinned local pending AskQuestion projection",
        gap: "pane-only wait; no durable RimZ ask or safe answer API",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no safe native reply API; answer in Cursor's pane",
    },
    compaction: ConcernCoverage::Partial {
        via: "preCompact opens; the next lifecycle signal closes the bracket",
        gap: "no post-compaction event; landing status and phase held",
    },
    subagents: ConcernCoverage::Wired {
        via: "subagentStart/subagentStop with exact child and parent ids",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    session_end: ConcernCoverage::Wired { via: "sessionEnd" },
    idle_notification: ConcernCoverage::Partial {
        via: "turn boundaries + stall window",
        gap: "no idle Notification hook",
    },
    context_usage: ConcernCoverage::Wired {
        via: "statusline window/fill/token composition; preCompact and stop fallback",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "model-priced response/stop-hook accumulation",
        gap: "live session only; no historical Cursor usage ledger",
    },
    rich_context: ConcernCoverage::Wired {
        via: "command statusline payload",
    },
    hook_install: ConcernCoverage::Wired {
        via: "managed ~/.cursor/hooks.json + cli-config.json transaction",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "identity/plan/version are wired; no durable provider usage history",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const CURSOR_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "sessionStart",
    },
    turn_started: HookCoverage::Native {
        event: "beforeSubmitPrompt",
    },
    turn_ended: HookCoverage::Native { event: "stop" },
    tool_used: HookCoverage::Native {
        event: "postToolUse",
    },
    awaiting_input: HookCoverage::Derived {
        via: "validated local pending AskQuestion state",
        gap: "no native hook; pane-only wait and answer surface",
    },
    subagent_started: HookCoverage::Native {
        event: "subagentStart",
    },
    subagent_stopped: HookCoverage::Native {
        event: "subagentStop",
    },
    compacting: HookCoverage::Native {
        event: "preCompact",
    },
    compaction_ended: HookCoverage::Derived {
        via: "next lifecycle signal closes the bracket in step + display-window expiry",
        gap: "no post-compaction event; landing status and phase held",
    },
    ended: HookCoverage::Native {
        event: "sessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const LIFECYCLE_EVENTS: &[&str] = &[
    "sessionStart",
    "beforeSubmitPrompt",
    "postToolUse",
    "postToolUseFailure",
    "afterAgentResponse",
    "stop",
    "sessionEnd",
    "preCompact",
    "subagentStart",
    "subagentStop",
];
const WIRED_EVENTS: &[&str] = LIFECYCLE_EVENTS;
pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source cursor";
pub(super) const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source cursor";
const RIMZ_STATUS_LINE_COMMAND: &str = "rimz statusline feed --source cursor";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source cursor";
const STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key_path: &["statusLine"],
        command: RIMZ_STATUS_LINE_COMMAND,
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::Only(&[
            "padding",
            "updateIntervalMs",
            "timeoutMs",
        ]),
        wrap_policy: super::managed_statusline::WrapPolicy::Any,
        required_for_install: false,
    };

#[derive(Clone, Debug, Default)]
pub struct CursorAdapter;

#[cfg(feature = "testkit")]
#[doc(hidden)]
pub fn discover_local_sessions_under(
    cursor_home: &Path,
    workspaces: &[&Path],
) -> Vec<LocalSessionObservation> {
    workspaces
        .iter()
        .flat_map(|workspace| session::discover_under(cursor_home, workspace))
        .collect()
}

impl AgentAdapter for CursorAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CURSOR_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_agent_hook(event_name, None, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        WIRED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};
        vec![
            ClassificationSample::new(
                "sessionStart",
                test_json!({ "conversation_id": "c1", "session_id": "c1", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "beforeSubmitPrompt",
                test_json!({ "conversation_id": "c1", "prompt": "fix it", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUse",
                test_json!({ "conversation_id": "c1", "tool_name": "Write", "cwd": "/tmp", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUseFailure",
                test_json!({ "conversation_id": "c1", "tool_name": "Shell", "failure_type": "error", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "afterAgentResponse",
                test_json!({ "conversation_id": "c1", "text": "done", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "stop",
                test_json!({ "conversation_id": "c1", "status": "completed", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "sessionEnd",
                test_json!({ "conversation_id": "c1", "reason": "quit", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "preCompact",
                test_json!({ "conversation_id": "c1", "trigger": "manual", "context_usage_percent": 83.2, "context_window_size": 200000, "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "subagentStart",
                test_json!({ "subagent_id": "child-1", "parent_conversation_id": "c1", "subagent_type": "generalPurpose", "task": "inspect hooks", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "subagentStop",
                test_json!({ "subagent_id": "child-1", "parent_conversation_id": "c1", "subagent_type": "generalPurpose", "status": "completed", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn turn_cost_fixture(&self) -> Option<super::TurnCostFixture> {
        Some(super::TurnCostFixture {
            event_name: "stop",
            payload: test_json!({
                "generation_id": "gen-1",
                "status": "completed",
                "model_id": "default",
                "input_tokens": 22_725,
                "output_tokens": 26,
                "cache_read_tokens": 8_704,
                "cache_write_tokens": 0
            }),
        })
    }

    #[cfg(test)]
    fn local_session_fixture(&self) -> Option<LocalSessionObservation> {
        Some(session::fixture_observation())
    }

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(LIFECYCLE_EVENTS.contains(&event_name).then(|| json!({})))
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        if matches!(event_name, "subagentStart" | "subagentStop") {
            return self.observe_subagent_lifecycle(event_name, payload, parsed);
        }
        let turn_usage = (event_name == "stop")
            .then(|| parsed.turn_usage())
            .flatten();
        let signal = match event_name {
            "sessionStart" => LifecycleSignal::Registered,
            "beforeSubmitPrompt" => LifecycleSignal::TurnStarted,
            "postToolUse" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
                native_key: None,
            },
            "stop" if parsed.stop_outcome() == payloads::StopOutcome::Aborted => {
                LifecycleSignal::TurnInterrupted
            }
            "stop" => LifecycleSignal::TurnEnded {
                errored: parsed.stop_outcome() == payloads::StopOutcome::Error,
                parked_on_background: false,
            },
            "sessionEnd" => LifecycleSignal::Ended,
            "preCompact" => LifecycleSignal::Compacting,
            _ => return None,
        };
        let agent_id = parsed
            .conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = prompt;
        let effort = parsed.model_param("effort").map(ToOwned::to_owned);
        observation.transcript_path = parsed.transcript_path;
        observation.launch.model = parsed
            .model_id
            .or(parsed.model)
            .map(statusline::normalize_model);
        observation.launch.effort = effort;
        observation.context_pct = parsed
            .context_usage_percent
            .filter(|value| value.is_finite())
            .map(|value| value.round().clamp(0.0, 100.0) as u8);
        observation.context_window = parsed.context_window_size;
        if event_name == "stop" {
            observation.fresh_input_tokens = turn_usage.and_then(|usage| usage.fresh_input);
            observation.output_tokens = turn_usage.and_then(|usage| usage.output);
            observation.cache_read_input_tokens = turn_usage.and_then(|usage| usage.cache_read);
            observation.cache_write_input_tokens = turn_usage.and_then(|usage| usage.cache_write);
        }
        Some(observation)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        let payload =
            serde_json::from_value::<statusline::StatuslinePayload>(payload.clone()).ok()?;
        let markers = payload
            .session_id
            .as_deref()
            .and_then(transcript::statusline_turn_markers);
        let mut context = payload.into_context(source, Timestamp::now());
        if let Some(markers) = markers {
            context.turn_complete = markers.turn_complete;
            context.turn_interrupted = markers.turn_interrupted;
            context.turn_error = markers.turn_error;
        }
        Some(context)
    }

    fn price_turn_locally(
        &self,
        event_name: &str,
        payload: &Value,
        prices: &PriceBook,
    ) -> Option<LocallyPricedTurnCost> {
        if !matches!(event_name, "afterAgentResponse" | "stop") {
            return None;
        }
        let parsed = payloads::parse_payload(payload);
        if event_name == "stop"
            && !matches!(
                parsed.status.as_deref(),
                Some("completed" | "aborted" | "error")
            )
        {
            return None;
        }
        let usage = parsed.turn_usage()?;
        let turn_id = parsed.generation_id?.trim().to_owned();
        if turn_id.is_empty() {
            return None;
        }
        let model = parsed.model_id.or(parsed.model)?;
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        let price_key = if model.eq_ignore_ascii_case("default") {
            "cursor-auto"
        } else {
            model
        };
        let price = prices.price(price_key)?;
        let cost_usd = price.cost(
            usage.fresh_input.unwrap_or(0),
            usage.output.unwrap_or(0),
            usage.cache_write.unwrap_or(0),
            0,
            usage.cache_read.unwrap_or(0),
            model.to_ascii_lowercase().ends_with("-fast"),
        );
        (cost_usd.is_finite() && cost_usd > 0.0)
            .then_some(LocallyPricedTurnCost { turn_id, cost_usd })
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe(self.descriptor())
    }

    fn observe_assistant_message(&self, event_name: &str, payload: &Value) -> Option<String> {
        (event_name == "afterAgentResponse")
            .then(|| payloads::parse_payload(payload).text)
            .flatten()
    }

    fn local_context_refresh(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        if let super::RefreshTrigger::Hook(event) = trigger
            && !matches!(
                event,
                "sessionStart"
                    | "beforeSubmitPrompt"
                    | "postToolUse"
                    | "postToolUseFailure"
                    | "afterAgentResponse"
                    | "stop"
                    | "preCompact"
            )
        {
            return None;
        }
        transcript::refresh(ctx)
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        transcript::resolve_transcript(session_id, None, prior_path)
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        Some(vec![bin, "--resume".to_owned(), session_id.to_owned()])
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        let mut argv = self
            .descriptor()
            .launch
            .launch_command(extra_args, prompt)?;
        argv[0] = bin;
        Some(argv)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install_into(
            &install::cursor_hooks_path()?,
            &install::cursor_cli_config_path()?,
        )
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview_at(
            &install::cursor_hooks_path()?,
            &install::cursor_cli_config_path()?,
        )
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall_from(
            &install::cursor_hooks_path()?,
            &install::cursor_cli_config_path()?,
        )
    }

    fn hooks_installed(&self) -> bool {
        let Ok(hooks_path) = install::cursor_hooks_path() else {
            return false;
        };
        let Ok(config_path) = install::cursor_cli_config_path() else {
            return false;
        };
        install::hooks_installed_at(&hooks_path) && install::statusline_installed_at(&config_path)
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::cursor_hooks_path().is_ok_and(|path| install::managed_artifacts_at(&path))
            || install::cursor_cli_config_path()
                .is_ok_and(|path| install::statusline_artifact_at(&path))
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        let root = install::read_existing_json(&install::cursor_cli_config_path().ok()?).ok()?;
        super::managed_statusline::wrapped_command(&root, &STATUS_LINE)
    }

    fn status_line_invocation(&self) -> super::StatusLineInvocation {
        super::StatusLineInvocation::DirectArgv
    }
}

impl CursorAdapter {
    fn observe_subagent_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
        parsed: payloads::CursorHookPayload,
    ) -> Option<AgentLifecycleObservation> {
        let signal = match event_name {
            "subagentStart" => LifecycleSignal::SubagentStarted,
            "subagentStop" => LifecycleSignal::SubagentStopped {
                errored: parsed.stop_outcome() == payloads::StopOutcome::Error,
            },
            _ => return None,
        };
        let (agent_id, parent_agent_id) = match resolve_subagent_identity(
            self.descriptor().kind,
            event_name,
            parsed.subagent_id.as_deref(),
            parsed.parent_conversation_id.as_deref(),
            payload,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => (agent_id, parent_agent_id),
            SubagentIdentity::Quarantined => return None,
        };

        let mut observation = AgentLifecycleObservation::new(Some(agent_id), signal)
            .with_worktree_from_payload(payload);
        observation.parent_agent_id = Some(parent_agent_id);
        let subagent_type = parsed.subagent_type.as_deref().and_then(non_empty_trimmed);
        observation.agent_name = subagent_type.clone();
        observation.launch.role = subagent_type;
        observation.task = sanitize_user_prompt(parsed.task.as_deref()).or_else(|| {
            (event_name == "subagentStop")
                .then(|| sanitize_user_prompt(parsed.description.as_deref()))
                .flatten()
        });
        observation.worktree_branch = parsed.git_branch;
        if event_name == "subagentStart" {
            observation.launch.model = parsed.subagent_model.map(statusline::normalize_model);
        } else {
            observation.transcript_path = parsed.agent_transcript_path;
        }
        Some(observation)
    }
}

#[cfg(test)]
mod tests;
