//! Cursor CLI hook adapter.
//!
//! Cursor's native hooks expose session, turn, tool, exit, and compaction-open
//! signals. They expose no local permission/question gate, machine-readable
//! spend, or post-compaction event, so those gaps remain explicit in the
//! descriptor rather than inferred from pane text.

mod account;
mod install;
mod payloads;
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
    HookInstallReport, HookUninstallReport, LocallyPricedTurnCost, PriceBook, Result,
    classify_agent_hook, locate_binary, sanitize_user_prompt,
};
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
        native_ask_ui: false,
        transcript_tail_context: true,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
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
    ],
    thread_key: ThreadKey::PerFile,
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
    user_question: ConcernCoverage::Unsupported {
        reason: "no local question hook; ACP-only",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no observable local ask surface",
    },
    compaction: ConcernCoverage::Partial {
        via: "preCompact opens; the next lifecycle signal closes the bracket",
        gap: "no post-compaction event; landing status and phase held",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "subagentStop omits the child id supplied by subagentStart",
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
    awaiting_input: HookCoverage::Absent {
        reason: "no local permission/question/plan hook; ACP-only",
    },
    subagent_started: HookCoverage::Absent {
        reason: "subagentStop has no child id",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "subagentStop has no child id",
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
];
const WIRED_EVENTS: &[&str] = LIFECYCLE_EVENTS;
pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source cursor";
pub(super) const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source cursor";
const RIMZ_STATUS_LINE_COMMAND: &str = "rimz statusline feed --source cursor";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source cursor";
const STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key: "statusLine",
        command: RIMZ_STATUS_LINE_COMMAND,
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::Only(&[
            "padding",
            "updateIntervalMs",
            "timeoutMs",
        ]),
    };

#[derive(Clone, Debug, Default)]
pub struct CursorAdapter;

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

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(LIFECYCLE_EVENTS.contains(&event_name).then(|| json!({})))
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        let turn_usage = (event_name == "stop")
            .then(|| parsed.turn_usage())
            .flatten();
        let signal = match event_name {
            "sessionStart" => LifecycleSignal::Registered,
            "beforeSubmitPrompt" => LifecycleSignal::TurnStarted,
            "postToolUse" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
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

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "sessionEnd"
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        Some(vec![bin, "--resume".to_owned(), session_id.to_owned()])
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Plan => vec!["--mode=plan".to_owned()],
            PermissionMode::Auto => vec!["--auto-review".to_owned()],
            PermissionMode::Yolo => vec![
                "--force".to_owned(),
                "--sandbox".to_owned(),
                "disabled".to_owned(),
            ],
        }
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/summarize")
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|model| !model.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        for (present, field) in [
            (
                preset
                    .effort
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "effort",
            ),
            (preset.system_prompt_file.is_some(), "system-prompt-file"),
            (
                preset.append_system_prompt_file.is_some(),
                "append-system-prompt-file",
            ),
        ] {
            if present {
                return Err(super::PresetErr::UnsupportedField {
                    agent: "cursor",
                    field,
                });
            }
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        (field == super::PresetField::Model)
            .then(|| super::PresetArgMatcher::Flag(vec!["--model".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        Some(super::positional_prompt_argv(&bin, extra_args, prompt))
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

#[cfg(test)]
mod tests;
